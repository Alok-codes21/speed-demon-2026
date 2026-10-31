use std::env;
use std::fs;
use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::path::Path;
use std::thread;

const HOURS: usize = 72;
const ENDPOINTS: usize = 1_842;
const GROUPS: usize = HOURS * ENDPOINTS;
const INPUT_LINES: usize = 3_100_000;

const READ_BUF: usize = 1 << 23; // 8 MiB streaming buffer
const TABLE_SIZE: usize = 4096; // power of two, > 2x ENDPOINTS (load ~0.45)
const SMALL_GROUP: usize = 64; // full sort below this, selection above
const HASH_K: u64 = 0x9E37_79B9_7F4A_7C15;

// ---------------------------------------------------------------- helpers

/// SWAR memchr: 8 bytes per step, no dependencies.
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

/// Fast non-cryptographic hash (8 bytes per step) replacing SipHash.
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

// The specification fixes the input span to these 72 UTC hours.
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

// ------------------------------------------------------------ aggregation

struct Agg {
    /// Open-addressing table: (hash tag, endpoint id + 1); id+1 == 0 means empty.
    table: Vec<(u32, u32)>,
    endpoints: Vec<Vec<u8>>,
    counts: Vec<u32>,
    errors: Vec<u32>,
    /// Packed (group_id << 32) | latency.
    values: Vec<u64>,
}

impl Agg {
    fn new() -> Self {
        Agg {
            table: vec![(0, 0); TABLE_SIZE],
            endpoints: Vec::with_capacity(ENDPOINTS),
            counts: vec![0; GROUPS],
            errors: vec![0; GROUPS],
            values: Vec::with_capacity(INPUT_LINES),
        }
    }

    #[cold]
    #[inline(never)]
    fn insert(&mut self, name: &[u8], tag: u32, slot: usize) -> usize {
        let id = self.endpoints.len();
        assert!(id < ENDPOINTS, "more distinct endpoints than specified");
        self.endpoints.push(name.to_vec());
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
            if t == tag && self.endpoints[(id1 - 1) as usize].as_slice() == name {
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
        // The timestamp is at least 13 bytes (indices up to 12 are read).
        let s1 = 13 + memchr(b' ', &line[13..]).expect("malformed line");
        let s2 = s1 + 1 + memchr(b' ', &line[s1 + 1..]).expect("malformed line");
        let s3 = s2 + 1 + memchr(b' ', &line[s2 + 1..]).expect("malformed line");
        let s4 = s3 + 1 + memchr(b' ', &line[s3 + 1..]).expect("malformed line");

        let hour = hour_slot(&line[..s1]);
        let status = parse_u32(&line[s3 + 1..s4]);
        let latency = parse_u32(&line[s4 + 1..]);
        let id = self.endpoint_id(&line[s2 + 1..s3]);

        let gid = hour * ENDPOINTS + id;
        self.counts[gid] += 1;
        if status.wrapping_sub(500) < 100 {
            self.errors[gid] += 1;
        }
        self.values.push(((gid as u64) << 32) | latency as u64);
    }
}

/// Streams the file in large chunks; lines are parsed in place (no copies).
fn ingest(path: &Path, agg: &mut Agg) -> io::Result<()> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; READ_BUF];
    let mut filled = 0usize;
    loop {
        let n = match file.read(&mut buf[filled..]) {
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        filled += n;

        let mut pos = 0;
        while let Some(nl) = memchr(b'\n', &buf[pos..filled]) {
            agg.line(&buf[pos..pos + nl]);
            pos += nl + 1;
        }
        if n == 0 {
            if pos < filled {
                agg.line(&buf[pos..filled]);
            }
            return Ok(());
        }
        buf.copy_within(pos..filled, 0);
        filled -= pos;
        if filled == buf.len() {
            let l = buf.len();
            buf.resize(l * 2, 0); // pathological very long line
        }
    }
}

// ------------------------------------------------------------ percentiles

/// Nearest-rank p50/p95/p99. Small groups: full sort. Large: nested
/// selection (O(n)) — p99 first, then p95 inside the lower part, then p50.
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
    let p50 = if r50 >= lo.len() {
        p95
    } else {
        *lo.select_nth_unstable(r50).1
    };
    [p50, p95, p99]
}

fn work(g0: usize, g1: usize, lat: &mut [u32], out: &mut [[u32; 3]], offsets: &[u32]) {
    let base = offsets[g0] as usize;
    for g in g0..g1 {
        let s = offsets[g] as usize - base;
        let e = offsets[g + 1] as usize - base;
        if s != e {
            out[g - g0] = percentiles(&mut lat[s..e]);
        }
    }
}

// ------------------------------------------------------------------- main

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let data_dir = args.get(1).map(String::as_str).unwrap_or("/data");
    let out_dir = args.get(2).map(String::as_str).unwrap_or("/out");

    let mut agg = Agg::new();
    ingest(&Path::new(data_dir).join("requests.log"), &mut agg)?;
    let Agg { endpoints, counts, errors, values, .. } = agg;

    let n = values.len();
    assert!(n < u32::MAX as usize);

    // Counting sort by group: offsets + one contiguous latency array.
    let mut offsets = vec![0u32; GROUPS + 1];
    let mut acc = 0u32;
    for g in 0..GROUPS {
        offsets[g] = acc;
        acc += counts[g];
    }
    offsets[GROUPS] = acc;

    let mut grouped = vec![0u32; n];
    {
        let mut cursor = offsets[..GROUPS].to_vec();
        for &p in &values {
            let g = (p >> 32) as usize;
            let c = &mut cursor[g];
            grouped[*c as usize] = p as u32;
            *c += 1;
        }
    }
    drop(values);

    // Percentiles in parallel over element-balanced, disjoint group ranges.
    let mut pct = vec![[0u32; 3]; GROUPS];
    let threads = if n < 200_000 {
        1
    } else {
        thread::available_parallelism().map(|x| x.get()).unwrap_or(1).min(16)
    };
    let mut bounds = vec![0usize];
    for t in 1..threads {
        let target = n * t / threads;
        let g = offsets.partition_point(|&o| (o as usize) < target).min(GROUPS);
        bounds.push(g);
    }
    bounds.push(GROUPS);

    {
        let offsets_ref: &[u32] = &offsets;
        thread::scope(|s| {
            let mut lat_rest: &mut [u32] = &mut grouped;
            let mut pct_rest: &mut [[u32; 3]] = &mut pct;
            for w in bounds.windows(2) {
                let (g0, g1) = (w[0], w[1]);
                let elems = (offsets_ref[g1] - offsets_ref[g0]) as usize;
                let (lat, lr) = lat_rest.split_at_mut(elems);
                lat_rest = lr;
                let (p, pr) = pct_rest.split_at_mut(g1 - g0);
                pct_rest = pr;
                if g0 != g1 {
                    s.spawn(move || work(g0, g1, lat, p, offsets_ref));
                }
            }
        });
    }

    // Endpoint IDs are by first appearance; emit in bytewise order.
    let mut order: Vec<usize> = (0..endpoints.len()).collect();
    order.sort_unstable_by(|&a, &b| endpoints[a].cmp(&endpoints[b]));

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

    let nonempty = counts.iter().filter(|&&c| c != 0).count();
    let mut out: Vec<u8> = Vec::with_capacity(nonempty * 100 + 64);

    for hour in 0..HOURS {
        let base = hour * ENDPOINTS;
        for &id in &order {
            let g = base + id;
            let c = counts[g];
            if c == 0 {
                continue;
            }
            out.extend_from_slice(&prefixes[hour]);
            out.extend_from_slice(&endpoints[id]);
            let e = errors[g];
            let ppm = (e as u64 * 1_000_000 / c as u64) as u32;
            let [p50, p95, p99] = pct[g];
            for v in [c, p50, p95, p99, e, ppm] {
                out.push(b' ');
                push_u32(&mut out, v);
            }
            out.push(b'\n');
        }
    }

    fs::create_dir_all(out_dir)?;
    fs::write(Path::new(out_dir).join("summary.txt"), &out)?;
    Ok(())
}