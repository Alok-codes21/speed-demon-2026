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
#[cfg(target_arch = "x86_64")]
static HAS_AVX2: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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

#[inline(always)]
fn is_tok(c: u8) -> bool {
    CLASS[c as usize] == 1
}

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
const EMPTY: u32 = u32::MAX;

struct Terms {
    bloom: Vec<u64>,
    slots: Vec<u32>, // (tag15 << 17) | tid ; EMPTY when free
    mask: usize,
    toff: Vec<u32>,
    tlen: Vec<u8>,
    arena: Vec<u8>,
    flags: Vec<u8>, // bit0: needs doc postings (BOOL term)
}
impl Terms {
    fn with_cap(n: usize) -> Self {
        let cap = (n * 2).next_power_of_two().max(16);
        Terms { bloom: vec![0u64; 32768], slots: vec![EMPTY; cap], mask: cap - 1, toff: vec![], tlen: vec![], arena: vec![], flags: vec![] }
    }
    fn insert(&mut self, w: &[u8], flag: u8) -> u32 {
        let h = unsafe { hash_tok(w.as_ptr(), w.len()) };
        let tag = ((h >> 48) as u32) & 0x7fff;
        let mut i = (h as usize) & self.mask;
        loop {
            let s = self.slots[i];
            if s == EMPTY {
                let tid = self.toff.len() as u32;
                self.slots[i] = (tag << 17) | tid;
                self.toff.push(self.arena.len() as u32);
                self.tlen.push(w.len() as u8);
                self.arena.extend_from_slice(w);
                self.flags.push(flag);
                let b = ((h >> 32) as usize) & 32767;
                self.bloom[b >> 6] |= 1u64 << (b & 63);
                return tid;
            }
            let tid = s & 0x1ffff;
            if (s >> 17) == tag
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
        let tag = ((h >> 48) as u32) & 0x7fff;
        let mut i = (h as usize) & self.mask;
        loop {
            let s = self.slots[i];
            if s == EMPTY {
                return NONE;
            }
            let tid = s & 0x1ffff;
            if (s >> 17) == tag && tok_eq(p, len, self.term_bytes(tid)) {
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
    pfx: Vec<u64>,
    has_len1: bool,
    qs: Vec<Q>,
    phr_tids: Vec<u32>,
    phr_bids: Vec<u32>,
    terms: Terms,
    bigrams: Bigrams,
    big1: Vec<u64>, // bitset: term id starts at least one query bigram
    n_terms: usize,
    n_bids: usize,
}

fn parse_queries(data: &[u8]) -> Queries {
    let mut terms = Terms::with_cap(80_000);
    let mut bigrams = Bigrams::with_cap(64_000);
    let mut qs = Vec::with_capacity(50_000);
    let mut phr_tids: Vec<u32> = Vec::with_capacity(80_000);
    let mut phr_bids: Vec<u32> = Vec::with_capacity(70_000);
    let mut big1 = vec![0u64; 2048]; // covers up to 131k term ids
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
                    big1[prev as usize >> 6] |= 1u64 << (prev & 63);
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
    // Two-byte folded-prefix filter: rejects most non-query tokens with one
    // L1-resident bit test before any hashing happens.
    let mut pfx = vec![0u64; 1024];
    let mut has_len1 = false;
    for tid in 0..n_terms as u32 {
        let tb = terms.term_bytes(tid);
        if tb.len() >= 2 {
            let k = ((tb[0] | 0x20) as usize) << 8 | (tb[1] | 0x20) as usize;
            pfx[k >> 6] |= 1u64 << (k & 63);
        } else {
            has_len1 = true;
        }
    }
    Queries { qs, phr_tids, phr_bids, terms, bigrams, big1, n_terms, n_bids, pfx, has_len1 }
}


/// SWAR memchr: first index >= i whose byte equals `byte`. The mapping has
/// 64 bytes of zero padding past the end, so unaligned u64 reads are safe.
#[inline(always)]
fn find_byte(buf: &Map, mut i: usize, byte: u8) -> usize {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGHS: u64 = 0x8080_8080_8080_8080;
    let pat = ONES * byte as u64;
    loop {
        let w = unsafe { (buf.ptr(i) as *const u64).read_unaligned() } ^ pat;
        let m = w.wrapping_sub(ONES) & !w & HIGHS;
        if m != 0 {
            return i + (m.trailing_zeros() >> 3) as usize;
        }
        i += 8;
    }
}

// ---------------------------------------------------------- corpus scan ----
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn handle_token(
    buf: &Map, s: usize, len: usize, q: &Queries,
    log: &mut Vec<u64>, n_terms: usize,
    docid: u32, prev: &mut u32,
) {
    unsafe {
        if len > 32 {
            *prev = NONE;
            return;
        }
        let p = buf.ptr(s);
        if len < 2 && !q.has_len1 {
            *prev = NONE;
            return;
        }
        let h = hash_tok(p, len);
        let b = ((h >> 32) as usize) & 32767;
        if q.terms.bloom[b >> 6] >> (b & 63) & 1 == 0 {
            *prev = NONE;
            return;
        }
        let tid = q.terms.probe(p, len, h);
        if tid != NONE {
            if q.terms.flags[tid as usize] & 1 != 0 {
                log.push(((tid as u64) << 32) | docid as u64);
            }
            if *prev != NONE && q.big1[*prev as usize >> 6] >> (*prev & 63) & 1 != 0 {
                let bid = q.bigrams.probe(*prev, tid);
                if bid != NONE {
                    log.push((((n_terms + bid as usize) as u64) << 32) | docid as u64);
                }
            }
            *prev = tid;
        } else {
            *prev = NONE;
        }
    }
}

#[allow(clippy::too_many_arguments)]
// ------------------------------------------------------- AVX2 tokenizer ----
/// Token byte classes: [a-zA-Z0-9]. 32 bytes -> 32-bit token mask.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn tok_mask32(p: *const u8) -> u32 {
    use core::arch::x86_64::*;
    let v = _mm256_loadu_si256(p as *const __m256i);
    let low = _mm256_or_si256(v, _mm256_set1_epi8(0x20));
    let bias = _mm256_set1_epi8(-128i8);
    let lb = _mm256_xor_si256(low, bias);
    let vb = _mm256_xor_si256(v, bias);
    let alpha = _mm256_and_si256(
        _mm256_cmpgt_epi8(lb, _mm256_set1_epi8((b'a' - 1 ^ 0x80) as i8)),
        _mm256_cmpgt_epi8(_mm256_set1_epi8((b'z' ^ 0x80).wrapping_add(1) as i8), lb),
    );
    let digit = _mm256_and_si256(
        _mm256_cmpgt_epi8(vb, _mm256_set1_epi8((b'0' - 1 ^ 0x80) as i8)),
        _mm256_cmpgt_epi8(_mm256_set1_epi8((b'9' ^ 0x80).wrapping_add(1) as i8), vb),
    );
    _mm256_movemask_epi8(_mm256_or_si256(alpha, digit)) as u32
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn scan_tokens_avx2(
    buf: &Map, start: usize, end: usize, q: &Queries,
    log: &mut Vec<u64>, n_terms: usize, docid: u32, prev: &mut u32,
) {
    let mut i = start;
    let mut ts = usize::MAX; // open token start (continues across 32B blocks)
    while i + 32 <= end {
        let m = tok_mask32(buf.ptr(i));
        if m == 0 {
            if ts != usize::MAX {
                handle_token(buf, ts, i - ts, q, log, n_terms, docid, prev);
                ts = usize::MAX;
            }
            i += 32;
            continue;
        }
        let mut pos = 0usize;
        if ts == usize::MAX {
            pos = m.trailing_zeros() as usize;
            ts = i + pos;
        }
        loop {
            let rl = (m >> pos).trailing_ones() as usize;
            if pos + rl == 32 {
                i += 32; // token continues into next block
                break;
            }
            handle_token(buf, ts, i + pos + rl - ts, q, log, n_terms, docid, prev);
            ts = usize::MAX;
            let sl = (!m >> (pos + rl)).trailing_ones() as usize;
            if pos + rl + sl == 32 {
                i += 32;
                break;
            }
            pos += rl + sl;
            ts = i + pos;
        }
    }
    if ts != usize::MAX {
        let mut j = i;
        while j < end && is_tok(buf.b(j)) {
            j += 1;
        }
        handle_token(buf, ts, j - ts, q, log, n_terms, docid, prev);
        i = j;
    }
    while i < end {
        if is_tok(buf.b(i)) {
            let s0 = i;
            i += 1;
            while i < end && is_tok(buf.b(i)) {
                i += 1;
            }
            handle_token(buf, s0, i - s0, q, log, n_terms, docid, prev);
        } else {
            i += 1;
        }
    }
}

fn scan_tokens_scalar_with_prev(
    buf: &Map, start: usize, end: usize, q: &Queries,
    log: &mut Vec<u64>, n_terms: usize,
    docid: u32, prev: &mut u32,
) {
    let mut i = start;
    while i < end {
        if is_tok(buf.b(i)) {
            let s = i;
            i += 1;
            while i < end && is_tok(buf.b(i)) {
                i += 1;
            }
            handle_token(buf, s, i - s, q, log, n_terms, docid, prev);
        } else {
            i += 1;
        }
    }
}

struct ScanRes {
    log: Vec<u64>, // (key << 32) | docid; key = tid, or n_terms + bid for bigrams
    offs: Vec<u32>,
    ndocs: u32,
}

fn scan_into(buf: &Map, mut i: usize, end: usize, q: &Queries, st: &mut ScanRes) {
    let log = &mut st.log;
    let n_terms = q.n_terms;
    let offs = &mut st.offs;
    // first line's doc id (ids are dense and ascending); peek, do not advance
    let mut docid: u32 = 0;
    {
        let tab = find_byte(buf, i, b'\t');
        for j in i..tab {
            docid = docid * 10 + (buf.b(j) - b'0') as u32;
        }
    }
    while i < end {
        let line_start = i;
        // skip id field + tab
        i = find_byte(buf, i, b'\t') + 1;
        offs.push(line_start as u32);
        // tokenize until the newline (a separator itself)
        let line_end = find_byte(buf, i, b'\n');
        let mut prev = NONE;
        #[cfg(target_arch = "x86_64")]
        if HAS_AVX2.load(std::sync::atomic::Ordering::Relaxed) {
            unsafe {
                scan_tokens_avx2(buf, i, line_end, q, log, n_terms, docid, &mut prev);
            }
            i = line_end + 1;
            docid += 1;
            continue;
        }
        scan_tokens_scalar_with_prev(buf, i, line_end, q, log, n_terms, docid, &mut prev);
        i = line_end + 1;
        docid += 1;
    }
    st.ndocs = docid;
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

/// One term's (or bigram's) postings as a growing delta-varint stream,
/// plus a skip table (every 32nd entry: absolute docid + byte offset of the
/// cursor state AFTER it) for galloping probes.
struct Stream {
    buf: Vec<u8>,
    skip: Vec<u64>,
    last: u32,
    n: u32,
}

/// Append one key's partitioned runs (per scan thread, ascending) to its stream,
/// recording a skip entry every 32 docs (cursor state after that entry).
fn encode_runs(st8: &mut Stream, key: usize, base: &[u32], part: &[u32], nthreads: usize) {
    let n = (base[key * nthreads + nthreads] - base[key * nthreads]) as usize;
    if n == 0 {
        return;
    }
    // ~2 bytes/entry estimate; reserve once so the buffer does not double-grow
    st8.buf.reserve(n * 2 + 8);
    let mut r = st8.n; // running entry index across segments
    let mut last = st8.last;
    let mut prev_d = u32::MAX; // adjacent-dup filter (per-doc dedup moved here from the scan)
    for t in 0..nthreads {
        let (a, b) = (base[key * nthreads + t] as usize, base[key * nthreads + t + 1] as usize);
        for &d in &part[a..b] {
            if d == prev_d {
                continue;
            }
            prev_d = d;
            put_varint(&mut st8.buf, d.wrapping_sub(last));
            last = d;
            r += 1;
            if r & 31 == 0 {
                st8.skip.push(((last as u64) << 32) | st8.buf.len() as u64);
            }
        }
    }
    st8.n = r;
    st8.last = last;
}

/// Membership probe against a stream via its skip table: jump to the last
/// skip entry below x, then decode forward. Used when the other side of the
/// op is much shorter than this stream.
fn probe(buf: &[u8], skip: &[u64], x: u32) -> bool {
    let (mut v, mut i) = if skip.is_empty() {
        (0u32, 0usize)
    } else {
        // last skip entry with docid < x (skip docids ascend)
        let mut lo = 0usize;
        let mut hi = skip.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if (skip[mid] >> 32) as u32 >= x {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        if lo == 0 {
            (0u32, 0usize)
        } else {
            let e = skip[lo - 1];
            ((e >> 32) as u32, e as u32 as usize)
        }
    };
    // decode at least one entry before comparing (the skip state is strictly < x)
    while i < buf.len() {
        let mut d = 0u32;
        let mut sh = 0;
        loop {
            let b = buf[i];
            i += 1;
            d |= ((b & 0x7f) as u32) << sh;
            if b < 0x80 {
                break;
            }
            sh += 7;
        }
        v = v.wrapping_add(d);
        if v >= x {
            return v == x;
        }
    }
    false
}

/// Forward cursor over a delta-varint doc list.
struct Cur<'a> {
    b: &'a [u8],
    i: usize,
    v: u32,
}
impl<'a> Cur<'a> {
    #[inline(always)]
    fn new(b: &'a [u8]) -> Cur<'a> {
        let mut c = Cur { b, i: 0, v: 0 };
        c.adv();
        c
    }
    #[inline(always)]
    fn adv(&mut self) {
        if self.i >= self.b.len() {
            self.v = NONE;
            return;
        }
        let mut x = 0u32;
        let mut sh = 0;
        loop {
            let b = self.b[self.i];
            self.i += 1;
            x |= ((b & 0x7f) as u32) << sh;
            if b < 0x80 {
                break;
            }
            sh += 7;
        }
        self.v = self.v.wrapping_add(x);
    }
    /// Advance until the current value is >= x (or the list is exhausted).
    #[inline(always)]
    fn seek(&mut self, x: u32) {
        while self.v < x {
            self.adv();
        }
    }
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

#[inline(always)]
fn intersect_stream(a: &[u32], l: &Stream, out: &mut Vec<u32>) {
    out.clear();
    if (a.len() as u64) * 24 < l.n as u64 {
        // heavy skew: probe the long stream per accumulator element
        out.extend(a.iter().copied().filter(|&x| probe(&l.buf, &l.skip, x)));
        return;
    }
    let mut cur = Cur::new(&l.buf);
    for &x in a {
        cur.seek(x);
        if cur.v == x {
            out.push(x);
        }
    }
}

#[inline(always)]
fn union_stream(a: &[u32], cur: &mut Cur, out: &mut Vec<u32>) {
    out.clear();
    out.reserve(a.len());
    let mut i = 0usize;
    while i < a.len() && cur.v != NONE {
        let y = cur.v;
        if a[i] < y {
            out.push(a[i]);
            i += 1;
        } else if y < a[i] {
            out.push(y);
            cur.adv();
        } else {
            out.push(a[i]);
            i += 1;
            cur.adv();
        }
    }
    out.extend_from_slice(&a[i..]);
    while cur.v != NONE {
        out.push(cur.v);
        cur.adv();
    }
}

#[inline(always)]
fn diff_stream(a: &[u32], l: &Stream, out: &mut Vec<u32>) {
    out.clear();
    if (a.len() as u64) * 24 < l.n as u64 {
        out.extend(a.iter().copied().filter(|&x| !probe(&l.buf, &l.skip, x)));
        return;
    }
    let mut cur = Cur::new(&l.buf);
    for &x in a {
        cur.seek(x);
        if cur.v != x {
            out.push(x);
        }
    }
}

/// intersect two varint streams directly into out (cleared first)
fn intersect_ss(mut x: Cur, mut y: Cur, out: &mut Vec<u32>) {
    out.clear();
    while x.v != NONE && y.v != NONE {
        if x.v < y.v {
            x.adv();
        } else if y.v < x.v {
            y.adv();
        } else {
            out.push(x.v);
            x.adv();
            y.adv();
        }
    }
}

/// union two varint streams directly into out (cleared first)
fn union_ss(mut x: Cur, mut y: Cur, out: &mut Vec<u32>) {
    out.clear();
    while x.v != NONE && y.v != NONE {
        if x.v < y.v {
            out.push(x.v);
            x.adv();
        } else if y.v < x.v {
            out.push(y.v);
            y.adv();
        } else {
            out.push(x.v);
            x.adv();
            y.adv();
        }
    }
    while x.v != NONE {
        out.push(x.v);
        x.adv();
    }
    while y.v != NONE {
        out.push(y.v);
        y.adv();
    }
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
    let t0 = std::time::Instant::now();
    #[cfg(target_arch = "x86_64")]
    HAS_AVX2.store(is_x86_feature_detected!("avx2"), std::sync::atomic::Ordering::Relaxed);
    let data = std::env::args().nth(1).unwrap_or_else(|| "/data".into());
    let outdir = std::env::args().nth(2).unwrap_or_else(|| "/out".into());

    // queries (padded so the token hash always has 8 readable bytes)
    let mut qbuf = std::fs::read(format!("{data}/queries.txt")).unwrap();
    let qlen = qbuf.len();
    qbuf.resize(qlen + 64, 0);
    let q = parse_queries(&qbuf[..qlen]);
    let n_terms = q.n_terms;
    let n_bids = q.n_bids;

    eprintln!("[t +{:6.3}] parse", t0.elapsed().as_secs_f64());
    // corpus
    let (mp, mlen) = map_file(&format!("{data}/docs.txt"), 64);
    let docs = Map(mp, mlen);

    eprintln!("[t +{:6.3}] mmap", t0.elapsed().as_secs_f64());
    let nthreads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8).min(16);
    const SEG: usize = 16; // scan segments; each partitioned+merged+released before the next
    // chunk starts aligned to line boundaries: nthreads stripes per segment
    let nchunks = nthreads * SEG;
    let mut bounds = Vec::with_capacity(nchunks + 1);
    bounds.push(0usize);
    for t in 1..nchunks {
        let mut p = mlen * t / nchunks;
        while p < mlen && docs.b(p - 1) != b'\n' {
            p += 1;
        }
        bounds.push(p.min(mlen));
    }
    bounds.push(mlen);

    let qr = &q;
    let dr = &docs;
    let br = &bounds;
    let mut states: Vec<ScanRes> = (0..nthreads)
        .map(|_| ScanRes {
            log: Vec::new(),
            offs: Vec::new(),
            ndocs: 0,
        })
        .collect();
    let nkeys = n_terms + n_bids;
    let mut cnt: Vec<u32> = vec![0; nthreads * nkeys]; // per-seg: thread-major counts, then scatter cursors
    let mut base: Vec<u32> = vec![0; nthreads * nkeys + 1]; // key-major run starts in `part`, + total sentinel
    let mut part: Vec<u32> = Vec::new(); // partitioned docids for this segment
    let mut tstreams: Vec<Stream> = (0..n_terms).map(|_| Stream { buf: Vec::new(), skip: Vec::new(), last: 0, n: 0 }).collect();
    let mut bstreams: Vec<Stream> = (0..n_bids).map(|_| Stream { buf: Vec::new(), skip: Vec::new(), last: 0, n: 0 }).collect();
    let mut offs: Vec<u32> = Vec::with_capacity(1_200_000);
    let tsz = n_terms.div_ceil(nthreads);
    let bsz = n_bids.max(1).div_ceil(nthreads);
    for seg in 0..SEG {
        // parallel scan of this segment's stripes into the persistent states
        std::thread::scope(|scope| {
            for (t, stp) in states.iter_mut().enumerate() {
                let (lo, hi) = (br[seg * nthreads + t], br[seg * nthreads + t + 1]);
                if lo >= hi {
                    continue;
                }
                std::thread::Builder::new()
                    .stack_size(256 << 10)
                    .spawn_scoped(scope, move || scan_into(dr, lo, hi, qr, stp))
                    .unwrap();
            }
        });
        // global doc offsets, in doc order (stripes are contiguous doc ranges)
        for s2 in states.iter_mut() {
            offs.extend_from_slice(&s2.offs);
            s2.offs.clear();
        }
        // partition this segment's pair logs by key (counting sort, stable per
        // thread, threads in doc order) then parallel-encode into streams.
        let st_ref: &Vec<ScanRes> = &states;
        for c in cnt.iter_mut() {
            *c = 0;
        }
        let cntp = cnt.as_mut_ptr() as usize;
        std::thread::scope(|scope| {
            for (t, st2) in st_ref.iter().enumerate() {
                let lg = &st2.log;
                std::thread::Builder::new()
                    .stack_size(256 << 10)
                    .spawn_scoped(scope, move || {
                        // each thread touches only its own row [t*nkeys, (t+1)*nkeys)
                        let row = unsafe { std::slice::from_raw_parts_mut((cntp as *mut u32).add(t * nkeys), nkeys) };
                        for &e in lg {
                            row[(e >> 32) as usize] += 1;
                        }
                    })
                    .unwrap();
            }
        });
        // key-major prefix sums; cnt becomes the thread-major scatter cursors
        let mut acc = 0u32;
        for key in 0..nkeys {
            for t in 0..nthreads {
                let c = cnt[t * nkeys + key];
                base[key * nthreads + t] = acc;
                cnt[t * nkeys + key] = acc;
                acc += c;
            }
        }
        base[nthreads * nkeys] = acc;
        part.clear();
        part.resize(acc as usize, 0u32);
        let curp = cnt.as_mut_ptr() as usize;
        let outp = part.as_mut_ptr() as usize;
        let plen = part.len();
        std::thread::scope(|scope| {
            for (t, st2) in st_ref.iter().enumerate() {
                let lg = &st2.log;
                std::thread::Builder::new()
                    .stack_size(256 << 10)
                    .spawn_scoped(scope, move || {
                        // each thread touches only cursor slots [t*nkeys + key]
                        // and writes only its own runs' positions in `part`
                        let crow = unsafe { std::slice::from_raw_parts_mut((curp as *mut u32).add(t * nkeys), nkeys) };
                        let out = unsafe { std::slice::from_raw_parts_mut(outp as *mut u32, plen) };
                        for &e in lg {
                            let key = (e >> 32) as usize;
                            let pos = crow[key] as usize;
                            out[pos] = e as u32;
                            crow[key] = pos as u32 + 1;
                        }
                    })
                    .unwrap();
            }
        });
        // parallel encode: each thread owns a term range + bigram range;
        // pass A sizes the varint bytes exactly, pass B encodes (no doubling slack)
        let baser: &Vec<u32> = &base;
        let part_ref: &Vec<u32> = &part;
        let mut t_it = tstreams.chunks_mut(tsz);
        let mut b_it = bstreams.chunks_mut(bsz);
        std::thread::scope(|scope| {
            for m in 0..nthreads {
                let tc = t_it.next().unwrap_or(&mut []);
                let bc = b_it.next().unwrap_or(&mut []);
                let tbase = m * tsz;
                let bbase = m * bsz;
                std::thread::Builder::new()
                    .stack_size(256 << 10)
                    .spawn_scoped(scope, move || {
                    for (l, st8) in tc.iter_mut().enumerate() {
                        let key = tbase + l;
                        encode_runs(st8, key, baser, part_ref, nthreads);
                    }
                    for (l, st8) in bc.iter_mut().enumerate() {
                        let key = n_terms + bbase + l;
                        encode_runs(st8, key, baser, part_ref, nthreads);
                    }
                    })
                    .unwrap();
            }
        });
        // release segment logs but keep capacity for the next segment
        for s2 in states.iter_mut() {
            s2.log.clear();
        }
    }
    drop(states);
    offs.push(mlen as u32);
    eprintln!("[t +{:6.3}] scan+merge", t0.elapsed().as_secs_f64());

    let st = State { q: &q, docs: &docs, offs: &offs, tstreams: &tstreams, bstreams: &bstreams };
    eprintln!("[t +{:6.3}] state", t0.elapsed().as_secs_f64());
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
            hs.push(std::thread::Builder::new()
                .stack_size(256 << 10)
                .spawn_scoped(scope, move || eval_range(&stc, lo, hi))
                .unwrap());
        }
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });

    eprintln!("[t +{:6.3}] eval (nq={})", t0.elapsed().as_secs_f64(), nq);
    std::fs::create_dir_all(&outdir).unwrap();
    let mut f = File::create(format!("{outdir}/results.txt")).unwrap();
    let mut all: Vec<u8> = Vec::with_capacity(2 << 20);
    for p in pieces.drain(..) {
        all.extend_from_slice(&p);
    }
    f.write_all(&all).unwrap();
}

#[derive(Clone, Copy)]
struct State<'a> {
    q: &'a Queries,
    docs: &'a Map,
    offs: &'a [u32],
    tstreams: &'a [Stream],
    bstreams: &'a [Stream],
}

impl<'a> State<'a> {
    #[inline(always)]
    fn tstream(&self, tid: u32) -> &'a Stream {
        &self.tstreams[tid as usize]
    }
    fn bstream(&self, bid: u32) -> &'a Stream {
        &self.bstreams[bid as usize]
    }
    #[inline(always)]
    fn tlist(&self, tid: u32) -> &'a [u8] {
        &self.tstreams[tid as usize].buf
    }
    fn blist(&self, bid: u32) -> &'a [u8] {
        &self.bstreams[bid as usize].buf
    }
    /// Map one doc token to its query-term id (NONE when not a query term).
    #[inline(always)]
    fn doc_tid(&self, p: *const u8, len: usize) -> u32 {
        if len > 32 {
            return NONE;
        }
        let h = unsafe { hash_tok(p, len) };
        let b = ((h >> 32) as usize) & 32767;
        if self.q.terms.bloom[b >> 6] >> (b & 63) & 1 == 0 {
            return NONE;
        }
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
                let mut k0 = 1usize;
                if bq.n >= 2 && bq.ops[0] == 0 {
                    let s0 = st.tstream(bq.tids[0]);
                    let s1 = st.tstream(bq.tids[1]);
                    if (s0.n as u64) * 24 < (s1.n as u64) {
                        decode_into(&s0.buf, &mut a);
                        a.retain(|&x| probe(&s1.buf, &s1.skip, x));
                    } else if (s1.n as u64) * 24 < (s0.n as u64) {
                        decode_into(&s1.buf, &mut a);
                        a.retain(|&x| probe(&s0.buf, &s0.skip, x));
                    } else {
                        intersect_ss(Cur::new(&s0.buf), Cur::new(&s1.buf), &mut a);
                    }
                    k0 = 2;
                } else if bq.n >= 2 && bq.ops[0] == 1 {
                    union_ss(Cur::new(st.tlist(bq.tids[0])), Cur::new(st.tlist(bq.tids[1])), &mut a);
                    k0 = 2;
                } else {
                    decode_into(st.tlist(bq.tids[0]), &mut a);
                }
                for k in k0..bq.n as usize {
                    let l = st.tstream(bq.tids[k]);
                    match bq.ops[k - 1] {
                        0 => {
                            if a.is_empty() || l.buf.is_empty() {
                                a.clear();
                                continue;
                            }
                            intersect_stream(&a, l, &mut c);
                            std::mem::swap(&mut a, &mut c);
                        }
                        1 => {
                            if l.buf.is_empty() {
                                continue;
                            }
                            union_stream(&a, &mut Cur::new(&l.buf), &mut c);
                            std::mem::swap(&mut a, &mut c);
                        }
                        _ => {
                            if a.is_empty() || l.buf.is_empty() {
                                continue;
                            }
                            diff_stream(&a, l, &mut c);
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
                decode_into(st.blist(bids[0]), &mut a);
                let mut ok = !a.is_empty();
                if ok && k > 2 {
                    // intersect the remaining bigram lists
                    for &bid in &bids[1..] {
                        let l = st.bstream(bid);
                        if l.buf.is_empty() {
                            a.clear();
                            ok = false;
                            break;
                        }
                        intersect_stream(&a, l, &mut c);
                        std::mem::swap(&mut a, &mut c);
                        if a.is_empty() {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    out.extend_from_slice(b"0 0\n");
                    continue;
                }
                if k == 2 {
                    write_result(&mut out, &a);
                    continue;
                }
                // verify exact sequence by re-tokenizing each candidate doc
                let tids = &st.q.phr_tids[off..off + k];
                b.clear();
                'docs: for &d in &a {
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
