use std::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
use std::ffi::c_void;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Barrier, Mutex, Once, OnceLock};
extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void;
    fn prctl(op: i32, a: u64, b: u64, c: u64, d: u64) -> i32;
    fn mallopt(param: i32, value: i32) -> i32;
}
const INF: u64 = u64::MAX;
const BLOB: usize = 512; // nodes per locality blob
const PART_BITS: u32 = 14; // nodes per part: 2^14, so offsets fit in 16 bits
const PAGE: usize = 512; // distances per page
#[derive(Clone, Copy)]
struct Raw<T>(*mut T);
unsafe impl<T> Send for Raw<T> {}
unsafe impl<T> Sync for Raw<T> {}
impl<T> Raw<T> {
    fn of(v: &mut [T]) -> Self {
        Raw(v.as_mut_ptr())
    }
    unsafe fn at(self, i: usize) -> *mut T {
        self.0.add(i)
    }
}
fn prefetch<T>(p: *const T) {
    unsafe { _mm_prefetch::<_MM_HINT_T0>(p as *const i8) }
}
fn map(path: &str) -> &'static [u8] {
    let file = std::fs::File::open(path).expect(path);
    let len = file.metadata().unwrap().len() as usize;
    unsafe {
        let p = mmap(std::ptr::null_mut(), len + 4096, 1, 0x22, -1, 0);
        let ok = len == 0 || mmap(p, len, 1, 0x12, file.as_raw_fd(), 0) == p;
        assert!(p as isize != -1 && ok, "mmap {path}");
        std::slice::from_raw_parts(p as *const u8, len)
    }
}
#[inline(always)]
fn number(text: &[u8], i: &mut usize) -> u32 {
    let w = u64::from_le_bytes(unsafe { *(text.as_ptr().add(*i) as *const [u8; 8]) });
    let stop = !w & 0x1010_1010_1010_1010;
    if stop == 0 {
        let mut v = 0u32;
        while let d @ b'0'..=b'9' = unsafe { *text.get_unchecked(*i) } {
            v = v * 10 + (d - b'0') as u32;
            *i += 1;
        }
        *i += 1;
        return v;
    }
    let len = stop.trailing_zeros() / 8; // digits before the separator
    *i += len as usize + 1;
    let mut v = (w << (64 - 8 * len)) & 0x0F0F_0F0F_0F0F_0F0F;
    v = (v * 10 + (v >> 8)) & 0x00FF_00FF_00FF_00FF;
    v = (v * 100 + (v >> 16)) & 0x0000_FFFF_0000_FFFF;
    ((v * 10000 + (v >> 32)) & 0xFFFF_FFFF) as u32
}
fn cuts(text: &[u8], from: usize, k: usize) -> Vec<usize> {
    let mut c = vec![text.len(); k + 1];
    c[0] = from;
    for t in 1..k {
        let mut x = (from + (text.len() - from) * t / k).max(c[t - 1]);
        while x < text.len() && text[x - 1] != b'\n' {
            x += 1;
        }
        c[t] = x;
    }
    c
}
type Job = &'static (dyn Fn(usize) + Sync);
static POOL: OnceLock<(Mutex<Option<Job>>, Barrier)> = OnceLock::new();
static STARTED: Once = Once::new();
fn on_threads<R: Send>(nt: usize, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    let (job, gate) = POOL.get_or_init(|| (Mutex::new(None), Barrier::new(nt)));
    STARTED.call_once(|| {
        for t in 1..nt {
            std::thread::spawn(move || loop {
                let (job, gate) = POOL.get().unwrap();
                gate.wait();
                let f = job.lock().unwrap().unwrap();
                f(t);
                gate.wait();
            });
        }
    });
    let out: Vec<Mutex<Option<R>>> = (0..nt).map(|_| Mutex::new(None)).collect();
    let run = |t: usize| *out[t].lock().unwrap() = Some(f(t));
    let run: &(dyn Fn(usize) + Sync) = &run;
    *job.lock().unwrap() = Some(unsafe { std::mem::transmute::<_, Job>(run) });
    gate.wait();
    run(0);
    gate.wait();
    out.into_iter().map(|r| r.into_inner().unwrap().unwrap()).collect()
}
fn share(n: usize, t: usize, nt: usize) -> std::ops::Range<usize> {
    n * t / nt..n * (t + 1) / nt
}
#[derive(Default)]
struct Heap(Vec<(u64, u32)>);
impl Heap {
    fn push(&mut self, k: u64, u: u32) {
        let h = &mut self.0;
        let mut i = h.len();
        h.push((k, u));
        unsafe {
            let p = h.as_mut_ptr();
            while i > 0 {
                let parent = (i - 1) / 4;
                let item = *p.add(parent);
                if item.0 <= k {
                    break;
                }
                *p.add(i) = item;
                i = parent;
            }
            *p.add(i) = (k, u);
        }
    }
    fn pop(&mut self) -> (u64, u32) {
        let x = self.0.pop().unwrap();
        let Some(&top) = self.0.first() else { return x };
        let (h, n, mut i) = (self.0.as_mut_ptr(), self.0.len(), 0);
        unsafe {
            while 4 * i + 1 < n {
                let c = 4 * i + 1;
                let end = n.min(c + 4);
                let mut m = c;
                if c + 1 < end && (*h.add(c + 1)).0 < (*h.add(m)).0 { m = c + 1; }
                if c + 2 < end && (*h.add(c + 2)).0 < (*h.add(m)).0 { m = c + 2; }
                if c + 3 < end && (*h.add(c + 3)).0 < (*h.add(m)).0 { m = c + 3; }
                if (*h.add(m)).0 >= x.0 {
                    break;
                }
                (*h.add(i), i) = (*h.add(m), m);
            }
            *h.add(i) = x;
        }
        top
    }
}
struct Dist {
    tab: Vec<u16>,
    used: Vec<u32>,
    pool: Vec<u64>,
}
impl Dist {
    fn new(n: usize) -> Self {
        Dist { tab: vec![0; n / PAGE + 1], used: Vec::new(), pool: Vec::new() }
    }
    fn get(&self, v: u32) -> u64 {
        match unsafe { *self.tab.get_unchecked(v as usize / PAGE) } as usize {
            0 => INF,
            pg => unsafe { *self.pool.get_unchecked((pg - 1) * PAGE + v as usize % PAGE) },
        }
    }
    fn at(&mut self, v: u32) -> &mut u64 {
        let i = v as usize / PAGE;
        if unsafe { *self.tab.get_unchecked(i) } == 0 {
            self.used.push(i as u32);
            self.tab[i] = self.used.len() as u16;
            let s = (self.used.len() - 1) * PAGE;
            self.pool.resize(self.pool.len().max(s + PAGE), INF);
            self.pool[s..s + PAGE].fill(INF);
        }
        let k = (self.tab[i] as usize - 1) * PAGE + v as usize % PAGE;
        unsafe { self.pool.get_unchecked_mut(k) }
    }
    fn reset(&mut self) {
        self.used.iter().for_each(|&i| self.tab[i as usize] = 0);
        self.used.clear();
    }
}
struct Filed {
    offs: Vec<u16>,
    words: Vec<u32>,
    chunks: Vec<Vec<u32>>, // part -> its chunks, in order
    len: Vec<usize>,       // part -> records
    heavy: Vec<Vec<u32>>,  // part -> its heavy weights, in order
}
const CHUNK: usize = 1024;
impl Filed {
    fn records(&self, p: usize) -> impl Iterator<Item = (usize, u32)> + '_ {
        self.chunks[p].iter().enumerate().flat_map(move |(j, &c)| {
            let r = c as usize * CHUNK..c as usize * CHUNK + (self.len[p] - j * CHUNK).min(CHUNK);
            self.offs[r.clone()].iter().map(|&o| o as usize).zip(self.words[r].iter().copied())
        })
    }
}
fn heavy_weight(heavy: &[(u32, u32)], e: u32) -> u32 {
    heavy[heavy.partition_point(|h| h.0 < e)].1
}
fn main() {
    unsafe {
        prctl(41, 1, 0, 0, 0); // PR_SET_THP_DISABLE
        mallopt(-3, 1 << 16); // M_MMAP_THRESHOLD
    }
    let nt = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 12);
    let qtext = map("/data/queries.txt");
    let mut queries = Vec::new();
    let mut i = 0;
    while i < qtext.len() {
        let s = number(qtext, &mut i);
        queries.push((s, number(qtext, &mut i)));
    }
    let text = map("/data/graph.txt");
    let mut i = 0;
    let n = number(text, &mut i) as usize;
    number(text, &mut i);
    let hb = 32 - (n as u32 | 1).leading_zeros(); // bits for a node id
    let (hmask, big) = ((1u32 << hb) - 1, u32::MAX >> hb);
    let sh = PART_BITS.min(hb);
    let parts = (n >> sh) + 1;
    let cut = cuts(text, i, nt);
    let filed: Vec<Filed> = on_threads(nt, |t| {
        let cap = (cut[t + 1] - cut[t]) / 6 + parts * CHUNK;
        let (mut at, mut next) = (vec![0; parts], 0); // where each part's next record goes
        let (offs, words) = (vec![0; cap], vec![0; cap]);
        let mut f = Filed { offs, words, chunks: vec![vec![]; parts], len: vec![0; parts], heavy: vec![vec![]; parts] };
        let mut i = cut[t];
        while i < cut[t + 1] {
            let u = number(text, &mut i);
            let v = number(text, &mut i);
            let mut w = number(text, &mut i);
            if u == v {
                continue; // a self-loop never shortens a path
            }
            let p = (u >> sh) as usize;
            if f.len[p] % CHUNK == 0 {
                (at[p], next) = (next * CHUNK, next + 1);
                f.chunks[p].push(next as u32 - 1);
            }
            if w >= big {
                f.heavy[p].push(w);
                w = big;
            }
            (f.offs[at[p]], f.words[at[p]]) = ((u & ((1 << sh) - 1)) as u16, v | w << hb);
            (at[p], f.len[p]) = (at[p] + 1, f.len[p] + 1);
        }
        f
    });
    let mut base = vec![0u32; parts + 1];
    for p in 0..parts {
        base[p + 1] = base[p] + filed.iter().map(|f| f.len[p] as u32).sum::<u32>();
    }
    let m = base[parts] as usize;
    let (mut first, mut link) = (vec![0u32; n + 1], vec![0u32; m]);
    first[n] = m as u32;
    let (fr, lr) = (Raw::of(&mut first), Raw::of(&mut link));
    let claim_part = AtomicUsize::new(0);
    let mut heavy: Vec<(u32, u32)> = on_threads(nt, |_| {
        let (mut heavy, mut cursor) = (Vec::new(), vec![0u32; 1 << sh]);
        loop {
            let p = claim_part.fetch_add(1, Relaxed);
            if p >= parts {
                return heavy;
            }
            let lo = p << sh;
            let len = (n - lo).min(1 << sh);
            cursor.fill(0);
            filed.iter().for_each(|f| f.records(p).for_each(|(o, _)| cursor[o] += 1));
            let mut at = base[p];
            for (k, c) in cursor[..len].iter_mut().enumerate() {
                unsafe { *fr.at(lo + k) = at };
                (at, *c) = (at + *c, at);
            }
            for f in &filed {
                let mut h = f.heavy[p].iter();
                for (o, x) in f.records(p) {
                    let c = &mut cursor[o];
                    if x >> hb == big {
                        heavy.push((*c, *h.next().unwrap()));
                    }
                    unsafe { *lr.at(*c as usize) = x };
                    *c += 1;
                }
            }
        }
    })
    .concat();
    heavy.sort_unstable();
    drop(filed);
    let claimed: Vec<AtomicU64> = (0..n.div_ceil(64)).map(|_| AtomicU64::new(0)).collect();
    let claim = |v: u32| {
        let (i, bit) = (v as usize / 64, 1u64 << (v % 64));
        claimed[i].fetch_or(bit, Relaxed) & bit == 0
    };
    let (first_v, link_v) = (first, link);
    let (first, link) = (&first_v[..], &link_v[..]);
    let chains: Vec<Vec<u32>> = on_threads(nt, |t| {
        let (mut ord, mut seeds) = (Vec::with_capacity(n / nt + BLOB), Vec::new());
        let mut scan = share(n, t, nt).start;
        let ahead = |ord: &Vec<u32>, i: usize| {
            ord.get(i + 8).map(|&u| prefetch(&first[u as usize]));
            ord.get(i + 4).map(|&u| prefetch(link.as_ptr().wrapping_add(first[u as usize] as usize)));
        };
        loop {
            let mut seed = None;
            while let Some(v) = seeds.pop() {
                if claim(v) {
                    seed = Some(v);
                    break;
                }
            }
            while seed.is_none() && scan < n {
                if claim(scan as u32) {
                    seed = Some(scan as u32);
                }
                scan += 1;
            }
            let Some(seed) = seed else { return ord };
            let (blob, mut i) = (ord.len(), ord.len());
            ord.push(seed);
            while i < ord.len() && ord.len() - blob < BLOB {
                ahead(&ord, i);
                let u = ord[i] as usize;
                for e in first[u]..first[u + 1] {
                    let v = link[e as usize] & hmask;
                    if ord.len() - blob < BLOB && claim(v) {
                        ord.push(v);
                    }
                }
                i += 1;
            }
            for i in i..ord.len() {
                ahead(&ord, i);
                let u = ord[i] as usize;
                for e in first[u]..first[u + 1] {
                    let v = link[e as usize] & hmask;
                    if claimed[v as usize / 64].load(Relaxed) & (1u64 << (v % 64)) == 0 {
                        seeds.push(v);
                    }
                }
            }
        }
    });
    let mut id_v = vec![0u32; n];
    let mut start = vec![0u32; nt + 1];
    for t in 0..nt {
        start[t + 1] = start[t] + chains[t].len() as u32;
    }
    let ir = Raw::of(&mut id_v);
    on_threads(nt, |t| chains[t].iter().zip(start[t]..).for_each(|(&u, k)| unsafe { *ir.at(u as usize) = k }));
    drop((claimed, chains));
    let id = &id_v[..];
    let mut off = vec![0u32; n + 1];
    let or = Raw::of(&mut off);
    on_threads(nt, |t| share(n, t, nt).for_each(|u| unsafe { *or.at(id[u] as usize + 1) = first[u + 1] - first[u] }));
    for v in 0..n {
        off[v + 1] += off[v];
    }
    let mut adj = vec![0u32; m];
    let ar = Raw::of(&mut adj);
    let off = &off;
    let mut heavy2: Vec<(u32, u32)> = on_threads(nt, |t| {
        let (mut out, r) = (Vec::new(), share(n, t, nt));
        let mut hi = heavy.partition_point(|h| h.0 < first[r.start]);
        for u in r.clone() {
            if u + 8 < r.end {
                prefetch(&off[id[u + 8] as usize]);
                (first[u + 8]..first[u + 9]).for_each(|e| prefetch(&id[(link[e as usize] & hmask) as usize]));
            }
            if u + 4 < r.end {
                prefetch(unsafe { ar.at(off[id[u + 4] as usize] as usize) });
            }
            let mut j = off[id[u] as usize];
            for e in first[u]..first[u + 1] {
                let x = link[e as usize];
                if x >> hb == big {
                    out.push((j, heavy[hi].1));
                    hi += 1;
                }
                unsafe { *ar.at(j as usize) = id[(x & hmask) as usize] | (x & !hmask) };
                j += 1;
            }
        }
        out
    })
    .concat();
    heavy2.sort_unstable();
    let ends: Vec<(u32, u32)> = queries.iter().map(|&(s, t)| (id[s as usize], id[t as usize])).collect();
    std::thread::spawn(move || drop((heavy, first_v, link_v, id_v)));
    let adj = &adj;
    let mut roff = vec![0u32; n + 1];
    let rr = Raw::of(&mut roff);
    let bump = |i: usize| unsafe { AtomicU32::from_ptr(rr.at(i)).fetch_add(1, Relaxed) };
    on_threads(nt, |t| share(n, t, nt).for_each(|u| (off[u]..off[u + 1]).for_each(|e| { bump((adj[e as usize] & hmask) as usize + 1); })));
    for v in 0..n {
        roff[v + 1] += roff[v];
    }
    let mut radj = vec![0u32; m];
    let rar = Raw::of(&mut radj);
    let mut rheavy: Vec<(u32, u32)> = on_threads(nt, |t| {
        let r = share(n, t, nt);
        let mut out = Vec::new();
        let mut hi = heavy2.partition_point(|h| h.0 < off[r.start]);
        for u in r {
            for e in off[u]..off[u + 1] {
                let x = adj[e as usize];
                let j = bump((x & hmask) as usize);
                if x >> hb == big {
                    out.push((j, heavy2[hi].1));
                    hi += 1;
                }
                unsafe { *rar.at(j as usize) = u as u32 | (x & !hmask) };
            }
        }
        out
    })
    .concat();
    rheavy.sort_unstable();
    roff.copy_within(0..n, 1);
    roff[0] = 0;
    let mut order: Vec<usize> = (0..ends.len()).collect();
    order.sort_unstable_by_key(|&q| ends[q]);
    let mut groups: Vec<(usize, usize)> = Vec::with_capacity(order.len());
    let mut start = 0;
    while start < order.len() {
        let key = ends[order[start]];
        let mut end = start + 1;
        while end < order.len() && ends[order[end]] == key {
            end += 1;
        }
        groups.push((start, end));
        start = end;
    }
    let sides = [(&off[..], &adj[..], &heavy2[..]), (&roff[..], &radj[..], &rheavy[..])];
    let mut ans = vec![INF; ends.len()];
    let ansr = Raw::of(&mut ans);
    let next = AtomicUsize::new(0);
    on_threads(nt, |_| {
        let mut dist = [Dist::new(n), Dist::new(n)];
        let mut queue = [Heap::default(), Heap::default()];
        loop {
            let q = next.fetch_add(1, Relaxed);
            let Some(&(lo_q, hi_q)) = groups.get(q) else { return };
            let i = order[lo_q];
            let ((s0, t0), (s, t)) = (queries[i], ends[i]);
            if s0 == t0 {
                for &j in &order[lo_q..hi_q] { unsafe { *ansr.at(j) = 0 }; }
                continue;
            }
            if off[s as usize + 1] == off[s as usize]
                || roff[t as usize + 1] == roff[t as usize]
            {
                for &j in &order[lo_q..hi_q] {
                    unsafe { *ansr.at(j) = INF };
                }
                continue; // no outgoing source edge or no incoming target edge
            }
            let mut best = INF;
            *dist[0].at(s) = 0;
            queue[0].push(0, s);
            *dist[1].at(t) = 0;
            queue[1].push(0, t);
            while let (Some(a), Some(b)) = (queue[0].0.first(), queue[1].0.first()) {
                if a.0 + b.0 >= best {
                    break;
                }
                let k = (queue[1].0[0].0 < queue[0].0[0].0) as usize;
                let (go, ga, gh) = sides[k];
                let (a, b) = dist.split_at_mut(1);
                let (mine, other) = if k == 0 { (a, b) } else { (b, a) };
                let (d, u) = queue[k].pop();
                if d > mine[0].get(u) {
                    continue;
                }
                let lo = unsafe { *go.get_unchecked(u as usize) };
                let hi = unsafe { *go.get_unchecked(u as usize + 1) };
                for e in lo..hi {
                    let x = unsafe { *ga.get_unchecked(e as usize) };
                    let (v, w) = (x & hmask, x >> hb);
                    let w = if w == big { heavy_weight(gh, e) } else { w };
                    let nd = d + w as u64;
                    if nd < best {
                        let slot = mine[0].at(v);
                        if nd < *slot {
                            *slot = nd;
                            queue[k].push(nd, v);
                            prefetch(go.as_ptr().wrapping_add(v as usize));
                            best = best.min(nd.saturating_add(other[0].get(v)));
                        }
                    }
                }
            }
            for &j in &order[lo_q..hi_q] { unsafe { *ansr.at(j) = best }; }
            queue.iter_mut().for_each(|q| q.0.clear());
            dist.iter_mut().for_each(Dist::reset);
        }
    });
    let mut out = Vec::with_capacity(ans.len() * 12);
    for &value in &ans {
        let value = value as i64; // INF becomes -1; real distances fit i64.
        if value < 0 {
            out.extend_from_slice(b"-1\n");
            continue;
        }
        let mut digits = [0u8; 20];
        let mut x = value as u64;
        let mut p = digits.len();
        loop {
            p -= 1;
            digits[p] = b'0' + (x % 10) as u8;
            x /= 10;
            if x == 0 { break; }
        }
        out.extend_from_slice(&digits[p..]);
        out.push(b'\n');
    }
    std::fs::write("/out/distances.txt", out).unwrap();
}
