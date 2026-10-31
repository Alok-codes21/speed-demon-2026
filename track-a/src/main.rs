// Candidate: parallel ingest, zero dependencies, zero unsafe.
//
// Phase 1: W threads pull 4 MiB chunks from an atomic counter and read them
//          with pread (FileExt::read_at). Each thread owns its endpoint table
//          and per-hour record buckets -> no locks, no shared writes.
// Merge:   union local endpoint names, sort bytewise, map local id -> sorted id.
// Phase 2: threads pull hours from an atomic counter; each hour is counted,
//          counting-sorted by endpoint, percentiled and formatted independently.
//          Hours are concatenated in order, so output order is exact.
use std::env;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

const HOURS: usize = 72;
const ENDPOINTS: usize = 1_842;
const INPUT_LINES: usize = 3_100_000;
const READ_BUF: usize = 1 << 20;
const CHUNK: u64 = 4 << 20;
const TABLE_SIZE: usize = 4096;
const SMALL_GROUP: usize = 64;
const HASH_K: u64 = 0x9E37_79B9_7F4A_7C15;

#[inline(always)]
fn memchr(needle: u8, hay: &[u8]) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    let rep = LO.wrapping_mul(needle as u64);
    let n = hay.len();
    let mut i = 0;
    while i + 8 <= n {
        let w = u64::from_le_bytes(hay[i..i + 8].try_into().unwrap()) ^ rep;
        let z = w.wrapping_sub(LO) & !w & HI;
        if z != 0 {
            return Some(i + (z.trailing_zeros() / 8) as usize);
        }
        i += 8;
    }
    while i < n {
        if hay[i] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[inline(always)]
fn parse_u32(bytes: &[u8]) -> u32 {
    let mut v = 0u32;
    for &b in bytes {
        v = v.wrapping_mul(10).wrapping_add(b.wrapping_sub(b'0') as u32);
    }
    v
}

#[inline(always)]
fn hash(b: &[u8]) -> u64 {
    let mut h = (b.len() as u64).wrapping_mul(HASH_K);
    let mut it = b.chunks_exact(8);
    for c in &mut it {
        let w = u64::from_le_bytes(c.try_into().unwrap());
        h = (h.rotate_left(23) ^ w).wrapping_mul(HASH_K);
    }
    let r = it.remainder();
    if !r.is_empty() {
        let mut t = [0u8; 8];
        t[..r.len()].copy_from_slice(r);
        h = (h.rotate_left(23) ^ u64::from_le_bytes(t)).wrapping_mul(HASH_K);
    }
    h = (h ^ (h >> 32)).wrapping_mul(HASH_K);
    h ^ (h >> 29)
}

#[inline(always)]
fn hour_slot(ts: &[u8]) -> usize {
    let d = |i: usize| ts[i].wrapping_sub(b'0') as u32;
    let month = d(5) * 10 + d(6);
    let day = d(8) * 10 + d(9);
    let hour = d(11) * 10 + d(12);
    let base: i32 = match (month, day) {
        (10, 30) => -18,
        (10, 31) => 6,
        (11, 1) => 30,
        (11, 2) => 54,
        _ => panic!("timestamp outside the specified 72-hour span"),
    };
    let slot = base + hour as i32;
    assert!((slot as u32) < HOURS as u32, "hour outside the specified span");
    slot as usize
}

#[inline(always)]
fn push_u32(out: &mut Vec<u8>, mut v: u32) {
    let mut buf = [0u8; 10];
    let mut i = 10;
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[i..]);
}

#[inline(always)]
fn percentiles(v: &mut [u32]) -> [u32; 3] {
    let n = v.len();
    let r50 = (50 * n + 99) / 100 - 1;
    let r95 = (95 * n + 99) / 100 - 1;
    let r99 = (99 * n + 99) / 100 - 1;
    if n <= SMALL_GROUP {
        v.sort_unstable();
        return [v[r50], v[r95], v[r99]];
    }
    let p99 = *v.select_nth_unstable(r99).1;
    let (p95, lo): (u32, &mut [u32]) = if r95 == r99 {
        (p99, &mut v[..r99])
    } else {
        let low = &mut v[..r99];
        let p = *low.select_nth_unstable(r95).1;
        (p, &mut low[..r95])
    };
    let p50 = if r50 >= lo.len() { p95 } else { *lo.select_nth_unstable(r50).1 };
    [p50, p95, p99]
}

// ------------------------------------------------------------- phase 1

/// Record: latency (bits 0..32) | local endpoint id (32..48) | is_5xx (bit 48).
struct Local {
    table: Vec<(u32, u32)>,
    names: Vec<Vec<u8>>,
    buckets: Vec<Vec<u64>>, // one per hour
}

impl Local {
    fn new(cap: usize) -> Self {
        Local {
            table: vec![(0, 0); TABLE_SIZE],
            names: Vec::with_capacity(ENDPOINTS),
            buckets: (0..HOURS).map(|_| Vec::with_capacity(cap)).collect(),
        }
    }

    #[cold]
    #[inline(never)]
    fn insert(&mut self, name: &[u8], tag: u32, slot: usize) -> usize {
        let id = self.names.len();
        assert!(id < ENDPOINTS, "more distinct endpoints than specified");
        self.names.push(name.to_vec());
        self.table[slot] = (tag, id as u32 + 1);
        id
    }

    #[inline(always)]
    fn endpoint_id(&mut self, name: &[u8]) -> usize {
        let h = hash(name);
        let tag = (h >> 32) as u32;
        let mut i = (h as usize) & (TABLE_SIZE - 1);
        loop {
            let (t, id1) = self.table[i];
            if id1 == 0 {
                return self.insert(name, tag, i);
            }
            if t == tag && self.names[(id1 - 1) as usize].as_slice() == name {
                return (id1 - 1) as usize;
            }
            i = (i + 1) & (TABLE_SIZE - 1);
        }
    }

    #[inline(always)]
    fn line(&mut self, mut line: &[u8]) {
        if let Some(&b'\r') = line.last() {
            line = &line[..line.len() - 1];
        }
        if line.is_empty() {
            return;
        }
        let s1 = 13 + memchr(b' ', &line[13..]).expect("malformed line");
        let s2 = s1 + 1 + memchr(b' ', &line[s1 + 1..]).expect("malformed line");
        let s3 = s2 + 1 + memchr(b' ', &line[s2 + 1..]).expect("malformed line");
        let s4 = s3 + 1 + memchr(b' ', &line[s3 + 1..]).expect("malformed line");

        let hour = hour_slot(&line[..s1]);
        let status = parse_u32(&line[s3 + 1..s4]);
        let latency = parse_u32(&line[s4 + 1..]);
        let id = self.endpoint_id(&line[s2 + 1..s3]);
        let err = (status.wrapping_sub(500) < 100) as u64;
        self.buckets[hour].push(latency as u64 | (id as u64) << 32 | err << 48);
    }
}

fn read_at_retry(file: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    loop {
        match file.read_at(buf, off) {
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            r => return r,
        }
    }
}

/// Processes every line whose first byte lies in [a, b).
fn process_range(file: &File, a: u64, b: u64, buf: &mut Vec<u8>, st: &mut Local) -> io::Result<()> {
    // Start one byte early so a line beginning exactly at `a` is recognised.
    let mut base = if a > 0 { a - 1 } else { 0 };
    let mut skipping = a > 0;
    let mut filled = 0usize;
    loop {
        let n = read_at_retry(file, &mut buf[filled..], base + filled as u64)?;
        filled += n;
        let mut pos = 0;
        while let Some(nl) = memchr(b'\n', &buf[pos..filled]) {
            if skipping {
                skipping = false;
                pos += nl + 1;
                continue;
            }
            if base + pos as u64 >= b {
                return Ok(());
            }
            st.line(&buf[pos..pos + nl]);
            pos += nl + 1;
        }
        if n == 0 {
            if !skipping && pos < filled && base + (pos as u64) < b {
                st.line(&buf[pos..filled]);
            }
            return Ok(());
        }
        buf.copy_within(pos..filled, 0);
        filled -= pos;
        base += pos as u64;
        if filled == buf.len() {
            let l = buf.len();
            buf.resize(l * 2, 0);
        }
    }
}

fn worker(file: &File, size: u64, next: &AtomicUsize, nchunks: usize, cap: usize) -> io::Result<Local> {
    let mut st = Local::new(cap);
    let mut buf = vec![0u8; READ_BUF];
    loop {
        let i = next.fetch_add(1, Ordering::Relaxed);
        if i >= nchunks {
            return Ok(st);
        }
        let a = i as u64 * CHUNK;
        let b = (a + CHUNK).min(size);
        process_range(file, a, b, &mut buf, &mut st)?;
    }
}

// ------------------------------------------------------------- phase 2

struct Scratch {
    counts: Vec<u32>,
    errors: Vec<u32>,
    offsets: Vec<u32>,
    cursor: Vec<u32>,
    lat: Vec<u32>,
}

impl Scratch {
    fn new() -> Self {
        Scratch {
            counts: vec![0; ENDPOINTS + 1],
            errors: vec![0; ENDPOINTS + 1],
            offsets: vec![0; ENDPOINTS + 2],
            cursor: vec![0; ENDPOINTS + 1],
            lat: Vec::new(),
        }
    }
}

fn hour_task(
    h: usize,
    locals: &[Local],
    maps: &[Vec<u32>],
    names: &[&[u8]],
    prefix: &[u8],
    sc: &mut Scratch,
) -> Vec<u8> {
    let ne = names.len();
    sc.counts[..ne].fill(0);
    sc.errors[..ne].fill(0);
    let mut total = 0usize;
    for (w, l) in locals.iter().enumerate() {
        let m = &maps[w];
        let bucket = &l.buckets[h];
        total += bucket.len();
        for &r in bucket {
            let g = m[((r >> 32) & 0xFFFF) as usize] as usize;
            sc.counts[g] += 1;
            sc.errors[g] += ((r >> 48) & 1) as u32;
        }
    }
    let mut acc = 0u32;
    let mut nonempty = 0usize;
    for g in 0..ne {
        sc.offsets[g] = acc;
        sc.cursor[g] = acc;
        acc += sc.counts[g];
        nonempty += (sc.counts[g] != 0) as usize;
    }
    sc.offsets[ne] = acc;

    sc.lat.clear();
    sc.lat.resize(total, 0);
    for (w, l) in locals.iter().enumerate() {
        let m = &maps[w];
        for &r in &l.buckets[h] {
            let g = m[((r >> 32) & 0xFFFF) as usize] as usize;
            sc.lat[sc.cursor[g] as usize] = r as u32;
            sc.cursor[g] += 1;
        }
    }

    let mut out = Vec::with_capacity(nonempty * 100 + 16);
    for g in 0..ne {
        let c = sc.counts[g];
        if c == 0 {
            continue;
        }
        let s = sc.offsets[g] as usize;
        let [p50, p95, p99] = percentiles(&mut sc.lat[s..s + c as usize]);
        let e = sc.errors[g];
        let ppm = (e as u64 * 1_000_000 / c as u64) as u32;
        out.extend_from_slice(prefix);
        out.extend_from_slice(names[g]);
        for v in [c, p50, p95, p99, e, ppm] {
            out.push(b' ');
            push_u32(&mut out, v);
        }
        out.push(b'\n');
    }
    out
}

// ---------------------------------------------------------------- main

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let data_dir = args.get(1).map(String::as_str).unwrap_or("/data");
    let out_dir = args.get(2).map(String::as_str).unwrap_or("/out");

    let file = File::open(Path::new(data_dir).join("requests.log"))?;
    let size = file.metadata()?.len();
    let nchunks = ((size + CHUNK - 1) / CHUNK) as usize;
    let threads = thread::available_parallelism()
        .map(|x| x.get())
        .unwrap_or(1)
        .min(16)
        .min(nchunks.max(1));
    let cap = INPUT_LINES * 5 / 4 / (HOURS * threads) + 16;

    // Phase 1: parallel parse.
    let next = AtomicUsize::new(0);
    let locals: Vec<Local> = thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| s.spawn(|| worker(&file, size, &next, nchunks, cap)))
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap())
            .collect::<io::Result<Vec<_>>>()
    })?;

    // Merge: global ids are ranks in bytewise-sorted endpoint order.
    let mut names: Vec<&[u8]> = locals
        .iter()
        .flat_map(|l| l.names.iter().map(|v| v.as_slice()))
        .collect();
    names.sort_unstable();
    names.dedup();
    assert!(names.len() <= ENDPOINTS);
    let maps: Vec<Vec<u32>> = locals
        .iter()
        .map(|l| {
            l.names
                .iter()
                .map(|n| names.binary_search(&n.as_slice()).unwrap() as u32)
                .collect()
        })
        .collect();

    let prefixes: Vec<Vec<u8>> = (0..HOURS)
        .map(|slot| {
            let (month, day, hour) = match slot {
                0..=5 => (10u32, 30u32, 18 + slot as u32),
                6..=29 => (10, 31, (slot - 6) as u32),
                30..=53 => (11, 1, (slot - 30) as u32),
                _ => (11, 2, (slot - 54) as u32),
            };
            format!("2026-{month:02}-{day:02}T{hour:02}:00:00Z ").into_bytes()
        })
        .collect();

    // Phase 2: one independent task per hour.
    let next_h = AtomicUsize::new(0);
    let results: Vec<Vec<(usize, Vec<u8>)>> = thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut sc = Scratch::new();
                    let mut res = Vec::new();
                    loop {
                        let h = next_h.fetch_add(1, Ordering::Relaxed);
                        if h >= HOURS {
                            return res;
                        }
                        res.push((h, hour_task(h, &locals, &maps, &names, &prefixes[h], &mut sc)));
                    }
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut outs: Vec<Vec<u8>> = vec![Vec::new(); HOURS];
    for r in results {
        for (h, o) in r {
            outs[h] = o;
        }
    }

    fs::create_dir_all(out_dir)?;
    let mut f = File::create(Path::new(out_dir).join("summary.txt"))?;
    for o in &outs {
        f.write_all(o)?;
    }
    Ok(())
}