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
const EMPTY: u64 = u64::MAX;

struct Terms {
    bloom: Vec<u64>,
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
        Terms { bloom: vec![0u64; 32768], slots: vec![EMPTY; cap], mask: cap - 1, toff: vec![], tlen: vec![], arena: vec![], flags: vec![] }
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
                let b = ((h >> 32) as usize) & 32767;
                self.bloom[b >> 6] |= 1u64 << (b & 63);
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
    pfx: Vec<u64>,
    has_len1: bool,
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
    Queries { qs, phr_tids, phr_bids, terms, bigrams, n_terms, n_bids, pfx, has_len1 }
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
    posts: &mut [Vec<u32>], bposts: &mut [Vec<u32>],
    last_doc: &mut [u32], blast: &mut [u32], docid: u32, prev: &mut u32,
) {
    unsafe {
        if len > 32 {
            *prev = NONE;
            return;
        }
        let p = buf.ptr(s);
        if len >= 2 {
            let k = ((*p | 0x20) as usize) << 8 | (*p.add(1) | 0x20) as usize;
            if q.pfx[k >> 6] >> (k & 63) & 1 == 0 {
                *prev = NONE;
                return;
            }
        } else if !q.has_len1 {
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
            if q.terms.flags[tid as usize] & 1 != 0 && last_doc[tid as usize] != docid {
                posts[tid as usize].push(docid);
                last_doc[tid as usize] = docid;
            }
            if *prev != NONE {
                let bid = q.bigrams.probe(*prev, tid);
                if bid != NONE && blast[bid as usize] != docid {
                    bposts[bid as usize].push(docid);
                    blast[bid as usize] = docid;
                }
            }
            *prev = tid;
        } else {
            *prev = NONE;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_tokens_scalar(
    buf: &Map, start: usize, end: usize, q: &Queries,
    posts: &mut [Vec<u32>], bposts: &mut [Vec<u32>],
    last_doc: &mut [u32], blast: &mut [u32], docid: u32,
) -> u32 {
    let mut prev = NONE;
    let mut i = start;
    while i < end {
        if is_tok(buf.b(i)) {
            let s = i;
            i += 1;
            while i < end && is_tok(buf.b(i)) {
                i += 1;
            }
            handle_token(buf, s, i - s, q, posts, bposts, last_doc, blast, docid, &mut prev);
        } else {
            i += 1;
        }
    }
    prev
}

struct ScanRes {
    posts: Vec<Vec<u32>>,  // per tid (only BOOL terms fill)
    bposts: Vec<Vec<u32>>, // per bid
    last_doc: Vec<u32>,
    blast: Vec<u32>,
    offs: Vec<u64>,
    ndocs: u32,
}

fn scan_into(buf: &Map, mut i: usize, end: usize, q: &Queries, st: &mut ScanRes) {
    let posts = &mut st.posts;
    let bposts = &mut st.bposts;
    let last_doc = &mut st.last_doc;
    let blast = &mut st.blast;
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
        offs.push(line_start as u64);
        // tokenize until the newline (a separator itself)
        let line_end = find_byte(buf, i, b'\n');
        scan_tokens_scalar(buf, i, line_end, q, posts, bposts, last_doc, blast, docid);
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

/// One term's (or bigram's) postings as a growing delta-varint stream.
struct Stream {
    buf: Vec<u8>,
    last: u32,
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

#[inline(always)]
fn intersect_stream(a: &[u32], cur: &mut Cur, out: &mut Vec<u32>) {
    out.clear();
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
fn diff_stream(a: &[u32], cur: &mut Cur, out: &mut Vec<u32>) {
    out.clear();
    for &x in a {
        cur.seek(x);
        if cur.v != x {
            out.push(x);
        }
    }
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
    let t0 = std::time::Instant::now();
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
    const SEG: usize = 8; // scan segments; each merged+released before the next
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
            posts: (0..n_terms).map(|_| Vec::new()).collect(),
            bposts: (0..n_bids).map(|_| Vec::new()).collect(),
            last_doc: vec![NONE; n_terms],
            blast: vec![NONE; n_bids],
            offs: Vec::new(),
            ndocs: 0,
        })
        .collect();
    let mut tstreams: Vec<Stream> = (0..n_terms).map(|_| Stream { buf: Vec::new(), last: 0 }).collect();
    let mut bstreams: Vec<Stream> = (0..n_bids).map(|_| Stream { buf: Vec::new(), last: 0 }).collect();
    let mut offs: Vec<u64> = Vec::with_capacity(1_200_000);
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
                scope.spawn(move || scan_into(dr, lo, hi, qr, stp));
            }
        });
        // global doc offsets, in doc order (stripes are contiguous doc ranges)
        for s2 in states.iter_mut() {
            offs.extend_from_slice(&s2.offs);
            s2.offs.clear();
        }
        // parallel stream merge: each thread owns a term range + bigram range
        let st_ref: &Vec<ScanRes> = &states;
        let mut t_it = tstreams.chunks_mut(tsz);
        let mut b_it = bstreams.chunks_mut(bsz);
        std::thread::scope(|scope| {
            for m in 0..nthreads {
                let tc = t_it.next().unwrap_or(&mut []);
                let bc = b_it.next().unwrap_or(&mut []);
                let tbase = m * tsz;
                let bbase = m * bsz;
                scope.spawn(move || {
                    for (l, st8) in tc.iter_mut().enumerate() {
                        let tid = tbase + l;
                        for sc in st_ref.iter() {
                            for &d in &sc.posts[tid] {
                                put_varint(&mut st8.buf, d - st8.last);
                                st8.last = d;
                            }
                        }
                    }
                    for (l, st8) in bc.iter_mut().enumerate() {
                        let bid = bbase + l;
                        for sc in st_ref.iter() {
                            for &d in &sc.bposts[bid] {
                                put_varint(&mut st8.buf, d - st8.last);
                                st8.last = d;
                            }
                        }
                    }
                });
            }
        });
        // release segment postings but keep capacity for the next segment
        for s2 in states.iter_mut() {
            for v in s2.posts.iter_mut().chain(s2.bposts.iter_mut()) {
                v.clear();
            }
        }
    }
    drop(states);
    offs.push(mlen as u64);
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
            hs.push(scope.spawn(move || eval_range(&stc, lo, hi)));
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
    offs: &'a [u64],
    tstreams: &'a [Stream],
    bstreams: &'a [Stream],
}

impl<'a> State<'a> {
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
                decode_into(st.tlist(bq.tids[0]), &mut a);
                for k in 1..bq.n as usize {
                    let l = st.tlist(bq.tids[k]);
                    match bq.ops[k - 1] {
                        0 => {
                            if a.is_empty() || l.is_empty() {
                                a.clear();
                                continue;
                            }
                            intersect_stream(&a, &mut Cur::new(l), &mut c);
                            std::mem::swap(&mut a, &mut c);
                        }
                        1 => {
                            if l.is_empty() {
                                continue;
                            }
                            union_stream(&a, &mut Cur::new(l), &mut c);
                            std::mem::swap(&mut a, &mut c);
                        }
                        _ => {
                            if a.is_empty() || l.is_empty() {
                                continue;
                            }
                            diff_stream(&a, &mut Cur::new(l), &mut c);
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
                        let l = st.blist(bid);
                        if l.is_empty() {
                            a.clear();
                            ok = false;
                            break;
                        }
                        intersect_stream(&a, &mut Cur::new(l), &mut c);
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
