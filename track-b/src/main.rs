// Speed Demon 2026, Track B (Index). Linux, Rust std-only, zero dependencies.
//
// Query-driven inverted index:
//   1. Parse queries.txt first; collect every distinct query term and every
//      consecutive term pair (bigram) the PHRASE queries use.
//   2. mmap docs.txt (the harness pre-faults /data, so mapping it costs no
//      accounted memory) and scan it once in parallel: an exact byte
//      tokenizer records deduplicated doc ids for BOOL terms and for query
//      bigrams only - the other ~85% of corpus vocabulary is skipped.
//   3. BOOL queries merge sorted doc-id lists strictly left to right.
//      PHRASE queries intersect their bigram lists (a 2-term phrase is one
//      bigram lookup) and re-tokenize the few surviving documents to verify
//      the exact sequence.

use std::fs::File;
use std::io::Write;
use std::os::unix::io::AsRawFd;

const NONE: u32 = u32::MAX;

// ---------------------------------------------------------------- mmap ----
unsafe extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
}
/// Map the whole file plus `pad` zero-filled bytes after it.
fn map_file(path: &str, pad: usize) -> (*const u8, usize) {
    let f = File::open(path).unwrap();
    let len = f.metadata().unwrap().len() as usize;
    let page = 4096usize;
    let flen = (len + page - 1) / page * page;
    let total = flen + pad.max(page);
    unsafe {
        let base = mmap(std::ptr::null_mut(), total, 0, 0x22, -1, 0); // PROT_NONE, MAP_PRIVATE|MAP_ANONYMOUS
        assert!(base as isize != -1, "reserve mmap failed");
        let r = mmap(base, flen, 1, 0x12, f.as_raw_fd(), 0); // PROT_READ, MAP_PRIVATE|MAP_FIXED
        assert_eq!(r, base, "file mmap failed");
        (base as *const u8, len)
    }
}

struct Map(*const u8, usize);
unsafe impl Sync for Map {}
impl Map {
    #[inline(always)]
    fn b(&self, i: usize) -> u8 {
        unsafe { *self.0.add(i) }
    }
    #[inline(always)]
    fn ptr(&self, i: usize) -> *const u8 {
        unsafe { self.0.add(i) }
    }
}

// ------------------------------------------------------- byte classes ----
const fn build_class() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let c = i as u8;
        if (c >= b'a' && c <= b'z') || (c >= b'0' && c <= b'9') || (c >= b'A' && c <= b'Z') {
            t[i] = 1;
        }
        i += 1;
    }
    t
}
static CLASS: [u8; 256] = build_class();

// ------------------------------------------------------------ hashing ----
// fxhash-style; tokens <= 32 bytes, read in 8-byte chunks. The 0x20 fold is
// exactly the spec's A-Z -> a-z fold on token bytes ([a-z0-9A-Z]).
const HMUL: u64 = 0x517c_c1b7_2722_0a95;
const FOLD64: u64 = 0x2020_2020_2020_2020;

/// Caller guarantees 8 readable bytes at p (file padding / staged buffers).
#[inline(always)]
unsafe fn hash_tok(p: *const u8, len: usize) -> u64 {
    let mut h = (len as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    if len >= 8 {
        let mut i = 0;
        while i + 8 <= len {
            let c = std::ptr::read_unaligned(p.add(i) as *const u64) | FOLD64;
            h = (h ^ c).wrapping_mul(HMUL);
            i += 8;
        }
        if i < len {
            let c = std::ptr::read_unaligned(p.add(len - 8) as *const u64) | FOLD64;
            h = (h ^ c).wrapping_mul(HMUL);
        }
    } else {
        let mask = (1u64 << (len * 8)) - 1;
        let c = (std::ptr::read_unaligned(p as *const u64) | FOLD64) & mask;
        h = (h ^ c).wrapping_mul(HMUL);
    }
    let h = (h ^ (h >> 29)).wrapping_mul(HMUL);
    if h == u64::MAX {
        1
    } else {
        h
    }
}

/// Case-folding byte compare of token at p (len bytes) against stored term.
#[inline(always)]
unsafe fn tok_eq(p: *const u8, len: usize, term: &[u8]) -> bool {
    if term.len() != len {
        return false;
    }
    if len < 8 {
        let mask = (1u64 << (len * 8)) - 1;
        let c = (std::ptr::read_unaligned(p as *const u64) | FOLD64) & mask;
        let t = std::ptr::read_unaligned(term.as_ptr() as *const u64) & mask;
        return c == t;
    }
    let mut i = 0;
    while i + 8 <= len {
        let c = std::ptr::read_unaligned(p.add(i) as *const u64) | FOLD64;
        let t = std::ptr::read_unaligned(term.as_ptr().add(i) as *const u64);
        if c != t {
            return false;
        }
        i += 8;
    }
    if i < len {
        let rem = len - i;
        let mask = (1u64 << (rem * 8)) - 1;
        let c = (std::ptr::read_unaligned(p.add(len - 8) as *const u64) | FOLD64) & mask;
        let t = std::ptr::read_unaligned(term.as_ptr().add(len - 8) as *const u64) & mask;
        if c != t {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------- term table ----
const EMPTY: u64 = u64::MAX;

struct Terms {
    slots: Vec<u64>, // (hash32 << 32) | tid ; EMPTY when free
    mask: usize,
    toff: Vec<u32>,
    tlen: Vec<u8>,
    arena: Vec<u8>,
    flags: Vec<u8>, // bit0: needs doc postings (BOOL term)
}
impl Terms {
    fn with_cap(n: usize) -> Self {
        let cap = (n * 2).next_power_of_two().max(16);
        Terms { slots: vec![EMPTY; cap], mask: cap - 1, toff: vec![], tlen: vec![], arena: vec![], flags: vec![] }
    }
    fn insert(&mut self, w: &[u8], flag: u8) -> u32 {
        let h = unsafe { hash_tok(w.as_ptr(), w.len()) };
        let tag = ((h >> 32) as u32) | 0; // high half as tag
        let mut i = (h as usize) & self.mask;
        loop {
            let s = self.slots[i];
            if s == EMPTY {
                let tid = self.toff.len() as u32;
                let tag = if tag == u32::MAX { 0 } else { tag };
                self.slots[i] = ((tag as u64) << 32) | tid as u64;
                self.toff.push(self.arena.len() as u32);
                self.tlen.push(w.len() as u8);
                self.arena.extend_from_slice(w);
                self.flags.push(flag);
                return tid;
            }
            let tid = (s & 0xffff_ffff) as u32;
            if (s >> 32) as u32 == tag
                && unsafe { tok_eq(w.as_ptr(), w.len(), self.term_bytes(tid)) }
            {
                self.flags[tid as usize] |= flag;
                return tid;
            }
            i = (i + 1) & self.mask;
        }
    }
    #[inline(always)]
    fn term_bytes(&self, tid: u32) -> &[u8] {
        &self.arena[self.toff[tid as usize] as usize..][..self.tlen[tid as usize] as usize]
    }
    /// Hot read-only probe; token bytes at p (8 readable bytes guaranteed).
    #[inline(always)]
    unsafe fn probe(&self, p: *const u8, len: usize, h: u64) -> u32 {
        let tag = (h >> 32) as u32;
        let mut i = (h as usize) & self.mask;
        loop {
            let s = self.slots[i];
            if s == EMPTY {
                return NONE;
            }
            let tid = (s & 0xffff_ffff) as u32;
            if (s >> 32) as u32 == tag && tok_eq(p, len, self.term_bytes(tid)) {
                return tid;
            }
            i = (i + 1) & self.mask;
        }
    }
}

// --------------------------------------------------------- bigram table ----
struct Bigrams {
    keys: Vec<u64>, // (tid1 << 32 | tid2) + 1 ; 0 when free
    vals: Vec<u32>, // bid per slot
    mask: usize,
    n: usize,
}
impl Bigrams {
    fn with_cap(n: usize) -> Self {
        let cap = (n * 2).next_power_of_two().max(16);
        Bigrams { keys: vec![0; cap], vals: vec![0; cap], mask: cap - 1, n: 0 }
    }
    #[inline(always)]
    fn mix(k: u64) -> usize {
        let h = k.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (h ^ (h >> 29)) as usize
    }
    fn insert(&mut self, t1: u32, t2: u32) -> u32 {
        let k = ((t1 as u64) << 32) | t2 as u64;
        let mut i = Self::mix(k) & self.mask;
        loop {
            let s = self.keys[i];
            if s == 0 {
                let bid = self.n as u32;
                self.keys[i] = k + 1;
                self.vals[i] = bid;
                self.n += 1;
                return bid;
            }
            if s == k + 1 {
                return self.vals[i];
            }
            i = (i + 1) & self.mask;
        }
    }
    #[inline(always)]
    fn probe(&self, t1: u32, t2: u32) -> u32 {
        let k = ((t1 as u64) << 32) | t2 as u64;
        let mut i = Self::mix(k) & self.mask;
        loop {
            let s = self.keys[i];
            if s == 0 {
                return NONE;
            }
            if s == k + 1 {
                return self.vals[i];
            }
            i = (i + 1) & self.mask;
        }
    }
}

// ------------------------------------------------------------- queries ----
struct BoolQ {
    n: u8,
    tids: [u32; 6],
    ops: [u8; 5], // 0=AND 1=OR 2=NOT
}
enum Q {
    Bool(BoolQ),
    Phrase { off: u32, boff: u32, len: u8 }, // into phr_tids / phr_bids
}

struct Queries {
    qs: Vec<Q>,
    phr_tids: Vec<u32>,
    phr_bids: Vec<u32>,
    terms: Terms,
    bigrams: Bigrams,
    n_terms: usize,
    n_bids: usize,
}

fn parse_queries(data: &[u8]) -> Queries {
    let mut terms = Terms::with_cap(80_000);
    let mut bigrams = Bigrams::with_cap(64_000);
    let mut qs = Vec::with_capacity(50_000);
    let mut phr_tids: Vec<u32> = Vec::with_capacity(80_000);
    let mut phr_bids: Vec<u32> = Vec::with_capacity(70_000);
    let mut pos = 0usize;
    while pos < data.len() {
        let eol = data[pos..].iter().position(|&b| b == b'\n').map(|d| pos + d).unwrap_or(data.len());
        let line = &data[pos..eol];
        pos = eol + 1;
        if line.is_empty() {
            continue;
        }
        if line[0] == b'B' {
            // BOOL t1 OP t2 ...
            let mut bq = BoolQ { n: 0, tids: [0; 6], ops: [0; 5] };
            let mut expect_term = true;
            for w in line[5..].split(|&b| b == b' ') {
                if expect_term {
                    bq.tids[bq.n as usize] = terms.insert(w, 1);
                    bq.n += 1;
                } else {
                    bq.ops[(bq.n - 1) as usize] = match w[0] {
                        b'A' => 0,
                        b'O' => 1,
                        _ => 2,
                    };
                }
                expect_term = !expect_term;
            }
            qs.push(Q::Bool(bq));
        } else {
            // PHRASE "t1 t2 ... tk"
            let inner = &line[8..line.len() - 1];
            let off = phr_tids.len() as u32;
            let boff = phr_bids.len() as u32;
            let mut prev = NONE;
            let mut k = 0u8;
            for w in inner.split(|&b| b == b' ') {
                let tid = terms.insert(w, 0);
                phr_tids.push(tid);
                if prev != NONE {
                    let bid = bigrams.insert(prev, tid);
                    phr_bids.push(bid);
                }
                prev = tid;
                k += 1;
            }
            qs.push(Q::Phrase { off, boff, len: k });
        }
    }
    let n_terms = terms.toff.len();
    let n_bids = bigrams.n;
    Queries { qs, phr_tids, phr_bids, terms, bigrams, n_terms, n_bids }
}

// ---------------------------------------------------------- corpus scan ----
struct ScanRes {
    posts: Vec<Vec<u32>>,  // per tid (only BOOL terms fill)
    bposts: Vec<Vec<u32>>, // per bid
    offs: Vec<u64>,
    ndocs: u32,
}

fn scan(buf: &Map, mut i: usize, end: usize, q: &Queries) -> ScanRes {
    let mut posts: Vec<Vec<u32>> = (0..q.n_terms).map(|_| Vec::new()).collect();
    let mut bposts: Vec<Vec<u32>> = (0..q.n_bids).map(|_| Vec::new()).collect();
    let mut last_doc = vec![NONE; q.n_terms];
    let mut blast = vec![NONE; q.n_bids];
    let mut offs: Vec<u64> = Vec::with_capacity(25_000);
    // first line's doc id (ids are dense and ascending); peek, do not advance
    let mut docid: u32 = 0;
    {
        let mut j = i;
        while buf.b(j) != b'\t' {
            docid = docid * 10 + (buf.b(j) - b'0') as u32;
            j += 1;
        }
    }
    while i < end {
        let line_start = i;
        // skip id field + tab
        while buf.b(i) != b'\t' {
            i += 1;
        }
        i += 1;
        offs.push(line_start as u64);
        let mut prev = NONE;
        // tokenize until the newline (a separator itself)
        loop {
            while CLASS[buf.b(i) as usize] == 0 {
                if buf.b(i) == b'\n' {
                    break;
                }
                i += 1;
            }
            if buf.b(i) == b'\n' {
                i += 1;
                break;
            }
            let s = i;
            while CLASS[buf.b(i) as usize] != 0 {
                i += 1;
            }
            let len = i - s;
            let mut tid = NONE;
            if len <= 32 {
                let h = unsafe { hash_tok(buf.ptr(s), len) };
                tid = unsafe { q.terms.probe(buf.ptr(s), len, h) };
            }
            if tid != NONE {
                if q.terms.flags[tid as usize] & 1 != 0 && last_doc[tid as usize] != docid {
                    posts[tid as usize].push(docid);
                    last_doc[tid as usize] = docid;
                }
                if prev != NONE {
                    let bid = q.bigrams.probe(prev, tid);
                    if bid != NONE && blast[bid as usize] != docid {
                        bposts[bid as usize].push(docid);
                        blast[bid as usize] = docid;
                    }
                }
            }
            prev = tid;
        }
        docid += 1;
    }
    for v in posts.iter_mut().chain(bposts.iter_mut()) {
        v.shrink_to_fit();
    }
    ScanRes { posts, bposts, offs, ndocs: docid }
}

// --------------------------------------------------------------- varint ----
#[inline(always)]
fn put_varint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Arena {
    chunks: Vec<Vec<u8>>, // per merge thread
}
struct LRef {
    chunk: u32,
    off: u32,
    len: u32,
}

fn get_slice<'a>(a: &'a Arena, r: &LRef) -> &'a [u8] {
    &a.chunks[r.chunk as usize][r.off as usize..][..r.len as usize]
}

/// Decode a delta-varint doc list into out (cleared first).
#[inline(always)]
fn decode_into(bytes: &[u8], out: &mut Vec<u32>) {
    out.clear();
    let mut v = 0u32;
    let mut i = 0usize;
    while i < bytes.len() {
        let mut x = 0u32;
        let mut sh = 0;
        loop {
            let b = bytes[i];
            i += 1;
            x |= ((b & 0x7f) as u32) << sh;
            if b < 0x80 {
                break;
            }
            sh += 7;
        }
        v += x;
        out.push(v);
    }
}

// ------------------------------------------------------- sorted set ops ----
#[inline(always)]
fn contains(hay: &[u32], x: u32) -> bool {
    // galloping search; good when probing a long list with a short one
    let mut step = 1usize;
    let mut lo = 0usize;
    while lo + step <= hay.len() && hay[lo + step - 1] < x {
        lo += step;
        step *= 2;
    }
    let hi = (lo + step).min(hay.len());
    hay[lo..hi].binary_search(&x).is_ok()
}

fn intersect_into(a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    out.clear();
    let (s, l) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if s.len() * 8 < l.len() {
        out.extend(s.iter().copied().filter(|&x| contains(l, x)));
    } else {
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            match a[i].cmp(&b[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    out.push(a[i]);
                    i += 1;
                    j += 1;
                }
            }
        }
    }
}

fn union_into(a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    out.clear();
    out.reserve(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
}

fn diff_into(a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    out.clear();
    if a.len() * 8 < b.len() {
        out.extend(a.iter().copied().filter(|&x| !contains(b, x)));
        return;
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
}

// -------------------------------------------------------------- output ----
#[inline(always)]
fn write_u64(out: &mut Vec<u8>, v: u64) {
    if v == 0 {
        out.push(b'0');
        return;
    }
    let mut tmp = [0u8; 20];
    let mut n = 0;
    let mut v = v;
    while v > 0 {
        tmp[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    while n > 0 {
        n -= 1;
        out.push(tmp[n]);
    }
}

fn write_result(out: &mut Vec<u8>, r: &[u32]) {
    write_u64(out, r.len() as u64);
    out.push(b' ');
    let mut sum = 0u64;
    for &x in r {
        sum += x as u64;
    }
    write_u64(out, sum);
    for &x in r.iter().take(10) {
        out.push(b' ');
        write_u64(out, x as u64);
    }
    out.push(b'\n');
}

// ---------------------------------------------------------------- main ----
fn main() {
    let data = std::env::args().nth(1).unwrap_or_else(|| "/data".into());
    let outdir = std::env::args().nth(2).unwrap_or_else(|| "/out".into());

    // queries (padded so the token hash always has 8 readable bytes)
    let mut qbuf = std::fs::read(format!("{data}/queries.txt")).unwrap();
    let qlen = qbuf.len();
    qbuf.resize(qlen + 64, 0);
    let q = parse_queries(&qbuf[..qlen]);
    let n_terms = q.n_terms;
    let n_bids = q.n_bids;

    // corpus
    let (mp, mlen) = map_file(&format!("{data}/docs.txt"), 64);
    let docs = Map(mp, mlen);

    let nthreads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8).min(16);
    // chunk starts aligned to line boundaries
    let mut bounds = Vec::with_capacity(nthreads + 1);
    bounds.push(0usize);
    for t in 1..nthreads {
        let mut s = mlen * t / nthreads;
        while s < mlen && docs.b(s - 1) != b'\n' {
            s += 1;
        }
        bounds.push(s.min(mlen));
    }
    bounds.push(mlen);

    let qr = &q;
    let dr = &docs;
    let br = &bounds;
    let scans: Vec<ScanRes> = std::thread::scope(|scope| {
        let mut hs = Vec::new();
        for t in 0..nthreads {
            if br[t] >= br[t + 1] {
                continue;
            }
            hs.push(scope.spawn(move || scan(dr, br[t], br[t + 1], qr)));
        }
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let nchunks = scans.len();

    // global doc offsets, in doc order (chunks are contiguous doc ranges)
    let total_docs: u32 = scans.iter().map(|s| s.ndocs).sum();
    let mut offs: Vec<u64> = Vec::with_capacity(total_docs as usize + 1);
    for s in &scans {
        offs.extend_from_slice(&s.offs);
    }
    offs.push(mlen as u64);

    // Merge postings: per term, concat chunk lists in chunk order (each is
    // ascending, chunk doc ranges are ascending, so the concat is sorted)
    // and delta-varint encode. Bigram lists concat into a flat u32 arena.
    let mut posts: Vec<Vec<Vec<u32>>> = Vec::with_capacity(nchunks);
    let mut bposts: Vec<Vec<Vec<u32>>> = Vec::with_capacity(nchunks);
    for s in scans {
        posts.push(s.posts);
        bposts.push(s.bposts);
    }
    let mut varint: Vec<u8> = Vec::with_capacity(48 << 20);
    let mut term_ref: Vec<LRef> = Vec::with_capacity(n_terms);
    let mut scratch: Vec<u32> = Vec::new();
    for tid in 0..n_terms {
        scratch.clear();
        for c in posts.iter_mut() {
            scratch.extend_from_slice(&c[tid]);
            c[tid] = Vec::new(); // free as we go
        }
        let off = varint.len() as u32;
        let mut prev = 0u32;
        for &d in &scratch {
            put_varint(&mut varint, d - prev);
            prev = d;
        }
        term_ref.push(LRef { chunk: 0, off, len: varint.len() as u32 - off });
    }
    drop(posts);
    let mut barena: Vec<u32> = Vec::new();
    let mut big_ref: Vec<(u32, u32)> = Vec::with_capacity(n_bids); // (off, len)
    for bid in 0..n_bids {
        let off = bposts_arena_len(&barena);
        for c in bposts.iter_mut() {
            barena.extend_from_slice(&c[bid]);
            c[bid] = Vec::new();
        }
        big_ref.push((off, (barena.len() as u32) - off));
    }
    drop(bposts);

    let tarena = Arena { chunks: vec![varint] };
    let st = State { q: &q, docs: &docs, offs: &offs, tarena: &tarena, term_ref: &term_ref, barena: &barena, big_ref: &big_ref };

    // Evaluate queries in parallel over contiguous ranges (order preserved).
    let nq = q.qs.len();
    let eth = nthreads.min(16);
    let chunk = (nq + eth - 1) / eth;
    let mut pieces: Vec<Vec<u8>> = std::thread::scope(|scope| {
        let mut hs = Vec::new();
        for t in 0..eth {
            let lo = t * chunk;
            if lo >= nq {
                break;
            }
            let hi = (lo + chunk).min(nq);
            let stc = st;
            hs.push(scope.spawn(move || eval_range(&stc, lo, hi)));
        }
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    std::fs::create_dir_all(&outdir).unwrap();
    let mut f = File::create(format!("{outdir}/results.txt")).unwrap();
    let mut all: Vec<u8> = Vec::with_capacity(2 << 20);
    for p in pieces.drain(..) {
        all.extend_from_slice(&p);
    }
    f.write_all(&all).unwrap();
}

fn bposts_arena_len(a: &Vec<u32>) -> u32 {
    a.len() as u32
}

#[derive(Clone, Copy)]
struct State<'a> {
    q: &'a Queries,
    docs: &'a Map,
    offs: &'a [u64],
    tarena: &'a Arena,
    term_ref: &'a [LRef],
    barena: &'a [u32],
    big_ref: &'a [(u32, u32)],
}

impl<'a> State<'a> {
    #[inline(always)]
    fn blist(&self, bid: u32) -> &'a [u32] {
        let (off, len) = self.big_ref[bid as usize];
        &self.barena[off as usize..][..len as usize]
    }
    /// Map one doc token to its query-term id (NONE when not a query term).
    #[inline(always)]
    fn doc_tid(&self, p: *const u8, len: usize) -> u32 {
        if len > 32 {
            return NONE;
        }
        let h = unsafe { hash_tok(p, len) };
        unsafe { self.q.terms.probe(p, len, h) }
    }
}

fn eval_range(st: &State, lo: usize, hi: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(64 << 10);
    let mut a: Vec<u32> = Vec::new();
    let mut b: Vec<u32> = Vec::new();
    let mut c: Vec<u32> = Vec::new();
    let mut toks: Vec<u32> = Vec::with_capacity(1200);
    for qi in lo..hi {
        match &st.q.qs[qi] {
            Q::Bool(bq) => {
                decode_into(get_slice(st.tarena, &st.term_ref[bq.tids[0] as usize]), &mut a);
                for k in 1..bq.n as usize {
                    let lr = &st.term_ref[bq.tids[k] as usize];
                    match bq.ops[k - 1] {
                        0 => {
                            if a.is_empty() || lr.len == 0 {
                                a.clear();
                                continue;
                            }
                            decode_into(get_slice(st.tarena, lr), &mut b);
                            intersect_into(&a, &b, &mut c);
                            std::mem::swap(&mut a, &mut c);
                        }
                        1 => {
                            if lr.len == 0 {
                                continue;
                            }
                            decode_into(get_slice(st.tarena, lr), &mut b);
                            union_into(&a, &b, &mut c);
                            std::mem::swap(&mut a, &mut c);
                        }
                        _ => {
                            if a.is_empty() {
                                continue;
                            }
                            if lr.len == 0 {
                                continue;
                            }
                            decode_into(get_slice(st.tarena, lr), &mut b);
                            diff_into(&a, &b, &mut c);
                            std::mem::swap(&mut a, &mut c);
                        }
                    }
                }
                write_result(&mut out, &a);
            }
            Q::Phrase { off, boff, len } => {
                let off = *off as usize;
                let boff = *boff as usize;
                let k = *len as usize;
                let bids = &st.q.phr_bids[boff..boff + k - 1];
                let mut cand: &[u32] = st.blist(bids[0]);
                let mut ok = !cand.is_empty();
                if ok && k > 2 {
                    // intersect the remaining bigram lists
                    a.clear();
                    a.extend_from_slice(cand);
                    for &bid in &bids[1..] {
                        let l = st.blist(bid);
                        if l.is_empty() {
                            a.clear();
                            ok = false;
                            break;
                        }
                        intersect_into(&a, l, &mut c);
                        std::mem::swap(&mut a, &mut c);
                        if a.is_empty() {
                            ok = false;
                            break;
                        }
                    }
                    cand = unsafe { std::mem::transmute::<&[u32], &[u32]>(&a) };
                }
                if !ok {
                    out.extend_from_slice(b"0 0\n");
                    continue;
                }
                if k == 2 {
                    write_result(&mut out, cand);
                    continue;
                }
                // verify exact sequence by re-tokenizing each candidate doc
                let tids = &st.q.phr_tids[off..off + k];
                b.clear();
                'docs: for &d in cand {
                    verify_tokens(st, d as usize, &mut toks);
                    let n = toks.len();
                    if n >= k {
                        for w in 0..=(n - k) {
                            if toks[w] == tids[0] && toks[w + 1..w + k] == tids[1..] {
                                b.push(d);
                                continue 'docs;
                            }
                        }
                    }
                }
                write_result(&mut out, &b);
            }
        }
    }
    out
}

/// Re-tokenize one document into query-term ids (NONE for non-query tokens).
fn verify_tokens(st: &State, docid: usize, toks: &mut Vec<u32>) {
    toks.clear();
    let s = st.offs[docid] as usize;
    let e = st.offs[docid + 1] as usize;
    let mut i = s;
    // skip id + tab
    while st.docs.b(i) != b'\t' {
        i += 1;
    }
    i += 1;
    while i + 1 < e {
        while i + 1 < e && CLASS[st.docs.b(i) as usize] == 0 {
            i += 1;
        }
        if i + 1 >= e {
            break;
        }
        let ts = i;
        while i + 1 < e && CLASS[st.docs.b(i) as usize] != 0 {
            i += 1;
        }
        toks.push(st.doc_tid(st.docs.ptr(ts), i - ts));
    }
}
