//! Speed Demon 2026, Track B (Index).
//!
//! Query-driven inverted index, std-only, no dependencies:
//!   1. Parse queries.txt first; collect every distinct term, flagged by
//!      whether BOOL doc lists and/or PHRASE positions are needed for it.
//!   2. Stream docs.txt in blocks (parallel ranges): a byte-exact tokenizer
//!      per the spec records deduplicated doc ids for BOOL terms and packed
//!      (doc, position) entries for PHRASE terms only.
//!   3. BOOL queries merge sorted doc-id lists strictly left to right.
//!      PHRASE queries iterate the rarest term's occurrences and binary-search
//!      the other terms' packed position lists for the expected offsets.
//!
//! Reads /data/docs.txt and /data/queries.txt, writes /out/results.txt.

use std::fs;
use std::io::Write;
use std::os::unix::fs::FileExt;

// Tokenizer: fold A-Z to a-z only; a token is a maximal run of [a-z0-9].
// Every other byte (including all bytes >= 0x80) is a separator.
const SEP: u8 = 0xFF;

const fn build_fold() -> [u8; 256] {
    let mut t = [SEP; 256];
    let mut i = 0usize;
    while i < 256 {
        let c = i as u8;
        if (c >= b'a' && c <= b'z') || (c >= b'0' && c <= b'9') {
            t[i] = c;
        } else if c >= b'A' && c <= b'Z' {
            t[i] = c + 32;
        }
        i += 1;
    }
    t
}
static FOLD: [u8; 256] = build_fold();

/// Query terms are 1..=32 bytes, so a longer token can never match one.
const MAX_TERM: usize = 32;

// Chunk-based token hash; deterministic across platforms, no libm.
// Folding is byte | 0x20, which is exactly the spec's A-Z -> a-z fold on
// token bytes ([a-z0-9A-Z] all keep or gain the 0x20 bit).
const HMUL: u64 = 0x517c_c1b7_2722_0a95;
const FOLD64: u64 = 0x2020_2020_2020_2020;

/// Hashes `len` bytes at `p`. Caller guarantees 32 readable bytes at `p`
/// (the scan buffer is padded; query words are staged through a pad).
#[inline(always)]
unsafe fn hash_token_ptr(p: *const u8, len: usize, fold: bool) -> u64 {
    let mut h = (len as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    if len >= 8 {
        let mut i = 0usize;
        while i + 8 <= len {
            let mut c = std::ptr::read_unaligned(p.add(i) as *const u64);
            if fold {
                c |= FOLD64;
            }
            h = (h ^ c).wrapping_mul(HMUL);
            i += 8;
        }
        if i < len {
            let mut c = std::ptr::read_unaligned(p.add(len - 8) as *const u64);
            if fold {
                c |= FOLD64;
            }
            h = (h ^ c).wrapping_mul(HMUL);
        }
    } else {
        let mask = (1u64 << (len * 8)) - 1;
        let mut c = std::ptr::read_unaligned(p as *const u64) & mask;
        if fold {
            c |= FOLD64 & mask;
        }
        h = (h ^ c).wrapping_mul(HMUL);
    }
    let h = (h ^ (h >> 29)).wrapping_mul(HMUL);
    if h == 0 || h == u64::MAX {
        1 // reserved sentinels
    } else {
        h
    }
}

fn hash_query_word(b: &[u8]) -> u64 {
    let mut pad = [0u8; 40];
    pad[..b.len()].copy_from_slice(b);
    unsafe { hash_token_ptr(pad.as_ptr(), b.len(), false) }
}

// Term table: open addressing over single u64 entries
//   [ hash32 : low 32 | term id : 24 | flags : 8 ]
// so a probe touches one cache line before any byte comparison.
// A 4M-bit summary filter lets the corpus scan skip nearly every miss
// without touching the table.
const FILTER_BITS: usize = 1 << 22;
const EMPTY: u64 = u64::MAX;
const FLAG_DOCS: u8 = 1;
const FLAG_POS: u8 = 2;

struct Terms {
    slots: Vec<u64>,
    arena: Vec<u8>,
    spans: Vec<(u32, u32)>, // (offset, len) into arena, per term id
    filter: Vec<u64>,
    used: usize,
}

impl Terms {
    fn new(min_cap: usize) -> Self {
        let cap = (min_cap * 2).next_power_of_two();
        Terms {
            slots: vec![EMPTY; cap],
            arena: Vec::new(),
            spans: Vec::new(),
            filter: vec![0; FILTER_BITS / 64],
            used: 0,
        }
    }
    #[inline(always)]
    fn mask(&self) -> usize {
        self.slots.len() - 1
    }
    #[inline(always)]
    fn h32(hash: u64) -> u32 {
        (hash ^ (hash >> 32)) as u32
    }
    #[inline(always)]
    fn bytes(&self, id: u32) -> &[u8] {
        let (off, len) = self.spans[id as usize];
        &self.arena[off as usize..(off + len) as usize]
    }
    #[inline(always)]
    fn filter_test(&self, hash: u64) -> bool {
        let k = (hash as usize) & (FILTER_BITS - 1);
        (self.filter[k >> 6] >> (k & 63)) & 1 == 1
    }
    /// Folded-compare lookup: matches stored folded bytes against raw token
    /// bytes folded with | 0x20 (exact on [a-z0-9A-Z]).
    #[inline(always)]
    fn get_folded(&self, hash: u64, ptr: *const u8, len: usize) -> Option<u64> {
        let h32 = Self::h32(hash);
        let mut i = (h32 as usize) & self.mask();
        loop {
            let e = self.slots[i];
            if e == EMPTY {
                return None;
            }
            if e as u32 == h32 {
                let (off, l) = self.spans[((e >> 32) & 0xFF_FFFF) as usize];
                if l as usize == len {
                    let sb = unsafe {
                        std::slice::from_raw_parts(self.arena.as_ptr().add(off as usize), len)
                    };
                    let rb = unsafe { std::slice::from_raw_parts(ptr, len) };
                    let mut eq = true;
                    let mut k = 0;
                    while k < len {
                        if sb[k] != rb[k] | 0x20 {
                            eq = false;
                            break;
                        }
                        k += 1;
                    }
                    if eq {
                        return Some(e);
                    }
                }
            }
            i = (i + 1) & self.mask();
        }
    }
    /// Full lookup; returns the raw entry (hash32 | id | flags).
    #[inline(always)]
    fn get(&self, hash: u64, bytes: &[u8]) -> Option<u64> {
        let h32 = Self::h32(hash);
        let mut i = (h32 as usize) & self.mask();
        loop {
            let e = self.slots[i];
            if e == EMPTY {
                return None;
            }
            if e as u32 == h32 && self.bytes(((e >> 32) & 0xFF_FFFF) as u32) == bytes {
                return Some(e);
            }
            i = (i + 1) & self.mask();
        }
    }
    fn insert(&mut self, hash: u64, bytes: &[u8]) -> u32 {
        if (self.used + 1) * 2 > self.slots.len() {
            self.grow();
        }
        let k = (hash as usize) & (FILTER_BITS - 1);
        self.filter[k >> 6] |= 1u64 << (k & 63);
        let h32 = Self::h32(hash);
        let mut i = (h32 as usize) & self.mask();
        loop {
            let e = self.slots[i];
            if e == EMPTY {
                let id = self.spans.len() as u32;
                self.slots[i] = h32 as u64 | (id as u64) << 32;
                self.spans.push((self.arena.len() as u32, bytes.len() as u32));
                self.arena.extend_from_slice(bytes);
                self.used += 1;
                return id;
            }
            if e as u32 == h32 && self.bytes(((e >> 32) & 0xFF_FFFF) as u32) == bytes {
                return ((e >> 32) & 0xFF_FFFF) as u32;
            }
            i = (i + 1) & self.mask();
        }
    }
    fn mark(&mut self, hash: u64, bytes: &[u8], flag: u8) {
        let h32 = Self::h32(hash);
        let mut i = (h32 as usize) & self.mask();
        loop {
            let e = self.slots[i];
            if e == EMPTY {
                return;
            }
            if e as u32 == h32 && self.bytes(((e >> 32) & 0xFF_FFFF) as u32) == bytes {
                self.slots[i] |= (flag as u64) << 56;
                return;
            }
            i = (i + 1) & self.mask();
        }
    }
    fn grow(&mut self) {
        let ncap = self.slots.len() * 2;
        let mut ns = vec![EMPTY; ncap];
        let mask = ncap - 1;
        for i in 0..self.slots.len() {
            let e = self.slots[i];
            if e != EMPTY {
                let mut j = ((e as u32) as usize) & mask;
                while ns[j] != EMPTY {
                    j = (j + 1) & mask;
                }
                ns[j] = e;
            }
        }
        self.slots = ns;
    }
    fn len(&self) -> usize {
        self.spans.len()
    }
}

// Queries.
const OP_AND: u8 = 0;
const OP_OR: u8 = 1;
const OP_NOT: u8 = 2;

enum Query {
    Bool { terms: Vec<u32>, ops: Vec<u8> },
    Phrase { terms: Vec<u32> },
}

fn parse_queries(data: &[u8], terms: &mut Terms) -> Vec<Query> {
    let mut qs = Vec::with_capacity(50_000);
    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(b"BOOL ") {
            let mut t: Vec<u32> = Vec::with_capacity(6);
            let mut ops: Vec<u8> = Vec::with_capacity(5);
            let mut expect_term = true;
            for w in line[5..].split(|&b| b == b' ') {
                if expect_term {
                    let h = hash_query_word(w);
                    t.push(terms.insert(h, w));
                    terms.mark(h, w, FLAG_DOCS);
                } else {
                    ops.push(match w {
                        b"AND" => OP_AND,
                        b"OR" => OP_OR,
                        b"NOT" => OP_NOT,
                        _ => panic!("bad BOOL operator in query"),
                    });
                }
                expect_term = !expect_term;
            }
            qs.push(Query::Bool { terms: t, ops });
        } else if line.starts_with(b"PHRASE \"") {
            let inner = &line[8..line.len() - 1];
            let mut t: Vec<u32> = Vec::with_capacity(8);
            for w in inner.split(|&b| b == b' ') {
                let h = hash_query_word(w);
                t.push(terms.insert(h, w));
                terms.mark(h, w, FLAG_POS);
            }
            qs.push(Query::Phrase { terms: t });
        } else {
            panic!("bad query line");
        }
    }
    qs
}

// Corpus scan (streaming). Per thread and term:
//   docs: deduplicated ascending doc ids (BOOL terms)
//   ents: packed (doc << POS_SHIFT) | pos, ascending (PHRASE terms)
//   ovf:  (tid, (doc << 32) | pos) for pos >= POS_LIMIT (never in practice)
// A tiny per-thread (term, doc) recency cache keeps hot vocabulary terms
// off the posting vectors.
const POS_SHIFT: u32 = 13;
const POS_LIMIT: u32 = 1 << POS_SHIFT; // max tokens in any doc is ~7k
const POS_MASK: u32 = POS_LIMIT - 1;
const RECENT: usize = 4096;

struct Post {
    docs: Vec<Vec<u32>>,
    ents: Vec<Vec<u32>>,
    ovf: Vec<(u32, u64)>,
}

/// Advances past separators, then folds one token into `tb`; returns the
/// token length (which may exceed `tb`, in which case it can never be a
/// query term). Returns 0 when no token remains.
#[inline(always)]
fn next_token(data: &[u8], i: &mut usize, tb: &mut [u8]) -> usize {
    let n = data.len();
    let mut j = *i;
    while j < n && FOLD[data[j] as usize] == SEP {
        j += 1;
    }
    let mut tl = 0usize;
    while j < n {
        let f = FOLD[data[j] as usize];
        if f == SEP {
            break;
        }
        if tl < tb.len() {
            tb[tl] = f;
        }
        tl += 1;
        j += 1;
    }
    *i = j;
    tl
}


// ---------------------------------------------------------------------------
// AVX2 text scanner: classifies 32 bytes per instruction into token/separator
// bits, then walks token runs. Folds via | 0x20 (exact on token bytes).
// ---------------------------------------------------------------------------
#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn emit_token(
    base: *const u8,
    start: usize,
    len: usize,
    pos: u32,
    id: u32,
    terms: &Terms,
    post: &mut Post,
    recent: &mut [(u32, u32)],
) {
    if len > MAX_TERM {
        return;
    }
    let h = hash_token_ptr(base.add(start), len, true);
    if !terms.filter_test(h) {
        return;
    }
    if let Some(e) = terms.get_folded(h, base.add(start), len) {
        let t = ((e >> 32) & 0xFF_FFFF) as usize;
        let fl = (e >> 56) as u8;
        if fl & FLAG_DOCS != 0 {
            let rc = &mut recent[t & (RECENT - 1)];
            if *rc != (t as u32, id) {
                if post.docs[t].last() != Some(&id) {
                    post.docs[t].push(id);
                }
                *rc = (t as u32, id);
            }
        }
        if fl & FLAG_POS != 0 {
            if pos < POS_LIMIT {
                post.ents[t].push((id << POS_SHIFT) | pos);
            } else {
                post.ovf.push((t as u32, ((id as u64) << 32) | pos as u64));
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn scan_text_avx2(
    text: &[u8],
    id: u32,
    terms: &Terms,
    post: &mut Post,
    recent: &mut [(u32, u32)],
) {
    use std::arch::x86_64::*;
    let base = text.as_ptr();
    let n = text.len();
    let fold = _mm256_set1_epi8(0x20);
    let x80 = _mm256_set1_epi8(-128);
    // signed-compare range tests via the xor-0x80 trick
    let ge_a = _mm256_set1_epi8((0x60u8 ^ 0x80) as i8); // > 'a'-1
    let le_z = _mm256_set1_epi8((0x7Bu8 ^ 0x80) as i8); // < 'z'+1
    let ge_0 = _mm256_set1_epi8((0x2Fu8 ^ 0x80) as i8); // > '0'-1
    let le_9 = _mm256_set1_epi8((0x3Au8 ^ 0x80) as i8); // < '9'+1

    // Pass 1: token-bit mask per 32-byte vector (pad bytes classify as
    // separators; the last vector is masked to the true length).
    let mut masks = [0u32; 2048];
    let nv = n.div_ceil(32);
    debug_assert!(nv <= 2048);
    let mut v = 0usize;
    while v < nv {
        let c = _mm256_loadu_si256(base.add(v * 32) as *const __m256i);
        let f = _mm256_xor_si256(_mm256_or_si256(c, fold), x80);
        let cx = _mm256_xor_si256(c, x80);
        let letter = _mm256_and_si256(_mm256_cmpgt_epi8(f, ge_a), _mm256_cmpgt_epi8(le_z, f));
        let digit = _mm256_and_si256(_mm256_cmpgt_epi8(cx, ge_0), _mm256_cmpgt_epi8(le_9, cx));
        let mut m = _mm256_movemask_epi8(_mm256_or_si256(letter, digit)) as u32;
        let rem = n - v * 32;
        if rem < 32 {
            m &= (1u32 << rem) - 1;
        }
        masks[v] = m;
        v += 1;
    }

    // Pass 2: walk token runs over the mask array.
    let mut pos: u32 = 0;
    let mut v = 0usize;
    let mut run_start: i64 = -1;
    while v < nv {
        let mut m = masks[v];
        let base_bit = v * 32;
        if run_start >= 0 {
            let lead = (!m).trailing_zeros() as usize; // leading 1-bits
            if lead == 32 {
                v += 1;
                continue; // whole vector is one continuing token
            }
            emit_token(base, run_start as usize, base_bit + lead - run_start as usize, pos, id, terms, post, recent);
            pos += 1;
            run_start = -1;
            m = if lead + 1 >= 32 { 0 } else { m & (!0u32 << (lead + 1)) };
        }
        while m != 0 {
            let s = m.trailing_zeros() as usize;
            let after = m >> s;
            let run = (!after).trailing_zeros() as usize;
            if s + run < 32 {
                emit_token(base, base_bit + s, run, pos, id, terms, post, recent);
                pos += 1;
                m = if s + run + 1 >= 32 { 0 } else { m & (!0u32 << (s + run + 1)) };
            } else {
                run_start = (base_bit + s) as i64;
                break;
            }
        }
        v += 1;
    }
    // A token may run right up to the end of the text (final vector fully
    // consumed); emit it here.
    if run_start >= 0 {
        emit_token(base, run_start as usize, n - run_start as usize, pos, id, terms, post, recent);
    }
}

/// Scans whole lines in `data`: decimal id, TAB, text, LF.
fn scan_lines(data: &[u8], terms: &Terms, post: &mut Post, recent: &mut [(u32, u32)], use_avx2: bool) {
    let mut tb = [0u8; MAX_TERM + 8];
    let n = data.len();
    let mut i = 0usize;
    while i < n {
        let mut id: u32 = 0;
        while data[i] != b'\t' {
            id = id * 10 + (data[i] - b'0') as u32;
            i += 1;
        }
        i += 1;
        let text_start = i;
        while i < n && data[i] != b'\n' {
            i += 1;
        }
        let text = &data[text_start..i];
        #[cfg(target_arch = "x86_64")]
        if use_avx2 && text.len() <= 65536 {
            unsafe { scan_text_avx2(text, id, terms, post, recent) };
            i += 1; // skip LF
            continue;
        }
        let tn = text.len();
        let mut j = 0usize;
        let mut pos: u32 = 0;
        while j < tn {
            let tl = next_token(text, &mut j, &mut tb);
            if tl == 0 {
                break;
            }
            if tl <= MAX_TERM {
                let h = unsafe { hash_token_ptr(tb.as_ptr(), tl, false) };
                if terms.filter_test(h) {
                    if let Some(e) = terms.get(h, &tb[..tl]) {
                        let t = ((e >> 32) & 0xFF_FFFF) as usize;
                        let fl = (e >> 56) as u8;
                        if fl & FLAG_DOCS != 0 {
                            let rc = &mut recent[t & (RECENT - 1)];
                            if *rc != (t as u32, id) {
                                if post.docs[t].last() != Some(&id) {
                                    post.docs[t].push(id);
                                }
                                *rc = (t as u32, id);
                            }
                        }
                        if fl & FLAG_POS != 0 {
                            if pos < POS_LIMIT {
                                post.ents[t].push((id << POS_SHIFT) | pos);
                            } else {
                                post.ovf.push((t as u32, ((id as u64) << 32) | pos as u64));
                            }
                        }
                    }
                }
            }
            pos += 1;
        }
        i += 1; // skip LF
    }
}

const BLOCK: usize = 1 << 22; // 4 MB streaming blocks

/// Streams [start, end) of docs.txt; both bounds sit right after an LF.
fn thread_scan(path: &str, start: usize, end: usize, terms: &Terms, n_terms: usize) -> Post {
    #[cfg(target_arch = "x86_64")]
    let use_avx2 = std::arch::is_x86_feature_detected!("avx2")
        && std::env::var_os("TRACKB_SCALAR").is_none();
    #[cfg(not(target_arch = "x86_64"))]
    let use_avx2 = false;
    let mut post = Post {
        docs: (0..n_terms).map(|_| Vec::new()).collect(),
        ents: (0..n_terms).map(|_| Vec::new()).collect(),
        ovf: Vec::new(),
    };
    let mut recent = vec![(u32::MAX, 0u32); RECENT];
    let f = fs::File::open(path).expect("open docs.txt");
    let mut buf = vec![0u8; BLOCK + (1 << 16)];
    let mut carry = 0usize; // leading partial line in buf
    let mut pos = start;
    let stats = std::env::var_os("TRACKB_STATS").is_some();
    let mut t_io = std::time::Duration::ZERO;
    let mut t_scan = std::time::Duration::ZERO;
    while pos < end {
        let want = BLOCK.min(end - pos);
        let tr = std::time::Instant::now();
        let n = f.read_at(&mut buf[carry..carry + want], pos as u64).expect("read docs.txt");
        if stats {
            t_io += tr.elapsed();
        }
        if n == 0 {
            break;
        }
        pos += n;
        let avail = carry + n;
        let cut = if pos >= end {
            avail
        } else {
            match buf[..avail].iter().rposition(|&b| b == b'\n') {
                Some(p) => p + 1,
                None => 0,
            }
        };
        if cut > 0 {
            let ts = std::time::Instant::now();
            scan_lines(&buf[..cut], terms, &mut post, &mut recent, use_avx2);
            if stats {
                t_scan += ts.elapsed();
            }
            buf.copy_within(cut..avail, 0);
            carry = avail - cut;
        }
    }
    debug_assert!(carry == 0);
    if stats {
        eprintln!("STATS thread_range=[{start},{end}) io={t_io:?} scan_lines={t_scan:?}");
    }
    // Release Vec growth slack before the merge so peak memory during
    // build_index is exact parts + index, not ~1.7x parts + index.
    for v in post.docs.iter_mut() {
        v.shrink_to_fit();
    }
    for v in post.ents.iter_mut() {
        v.shrink_to_fit();
    }
    post
}

fn chunk_ranges(file_len: usize, path: &str, nthreads: usize) -> Vec<(usize, usize)> {
    // Probe a small window around each nominal split for the next LF.
    let f = fs::File::open(path).expect("open docs.txt");
    let mut ranges = Vec::with_capacity(nthreads);
    let size = file_len / nthreads + 1;
    let mut start = 0usize;
    while start < file_len {
        let mut end = (start + size).min(file_len);
        if end < file_len {
            let mut probe = vec![0u8; 1 << 16];
            let mut at = end;
            loop {
                let n = f.read_at(&mut probe, at as u64).expect("read docs.txt");
                match probe[..n].iter().position(|&b| b == b'\n') {
                    Some(p) => {
                        end = at + p + 1;
                        break;
                    }
                    None => at += n,
                }
            }
        }
        ranges.push((start, end));
        start = end;
    }
    ranges
}

// Merged index: flat arenas with per-term spans.
struct Index {
    docs: Vec<u32>,
    d_spans: Vec<(u32, u32)>,
    ents: Vec<u32>, // packed (doc << POS_SHIFT) | pos
    e_spans: Vec<(u32, u32)>,
    ovf: Vec<u64>, // (doc << 32) | pos for pos >= POS_LIMIT
    o_spans: Vec<(u32, u32)>,
}

impl Index {
    #[inline(always)]
    fn docs(&self, tid: u32) -> &[u32] {
        let (off, len) = self.d_spans[tid as usize];
        &self.docs[off as usize..(off + len) as usize]
    }
    #[inline(always)]
    fn ents(&self, tid: u32) -> &[u32] {
        let (off, len) = self.e_spans[tid as usize];
        &self.ents[off as usize..(off + len) as usize]
    }
    #[inline(always)]
    fn ovf(&self, tid: u32) -> &[u64] {
        let (off, len) = self.o_spans[tid as usize];
        &self.ovf[off as usize..(off + len) as usize]
    }
}

fn build_index(parts: &mut Vec<Post>, n_terms: usize) -> Index {
    let mut d_spans = vec![(0u32, 0u32); n_terms];
    let mut e_spans = vec![(0u32, 0u32); n_terms];
    let mut o_spans = vec![(0u32, 0u32); n_terms];
    let d_total: usize = parts.iter().map(|p| p.docs.iter().map(Vec::len).sum::<usize>()).sum();
    let e_total: usize = parts.iter().map(|p| p.ents.iter().map(Vec::len).sum::<usize>()).sum();
    let o_total: usize = parts.iter().map(|p| p.ovf.len()).sum();
    let mut docs: Vec<u32> = Vec::with_capacity(d_total);
    let mut ents: Vec<u32> = Vec::with_capacity(e_total);
    let mut ovf: Vec<u64> = Vec::with_capacity(o_total);
    for p in parts.iter_mut() {
        p.ovf.sort_unstable();
    }
    // Exact spans first (prefix sums over per-thread lengths), then a single
    // copy pass; per-thread vecs are freed as they are taken.
    {
        let mut d_off = 0u32;
        let mut e_off = 0u32;
        for tid in 0..n_terms {
            d_spans[tid].0 = d_off;
            e_spans[tid].0 = e_off;
            for p in parts.iter() {
                d_off += p.docs[tid].len() as u32;
                e_off += p.ents[tid].len() as u32;
            }
            d_spans[tid].1 = d_off - d_spans[tid].0;
            e_spans[tid].1 = e_off - e_spans[tid].0;
        }
    }
    for tid in 0..n_terms {
        for p in parts.iter_mut() {
            docs.extend_from_slice(&std::mem::take(&mut p.docs[tid]));
            ents.extend_from_slice(&std::mem::take(&mut p.ents[tid]));
        }
    }
    // Per-term overflow spans (per-thread overflow lists are sorted by
    // (tid, doc); overflow is normally empty, so a simple scan suffices).
    if o_total > 0 {
        for tid in 0..n_terms as u32 {
            let s = ovf.len() as u32;
            for p in parts.iter() {
                for &(t, v) in &p.ovf {
                    if t == tid {
                        ovf.push(v);
                    }
                }
            }
            o_spans[tid as usize] = (s, ovf.len() as u32 - s);
        }
    }
    if std::env::var_os("TRACKB_STATS").is_some() {
        let mut counts: Vec<(u32, u32)> = (0..n_terms as u32)
            .map(|t| (t, e_spans[t as usize].1))
            .filter(|&(_, c)| c > 0)
            .collect();
        counts.sort_by_key(|&(_, c)| std::cmp::Reverse(c));
        let total: u64 = counts.iter().map(|&(_, c)| c as u64).sum();
        eprintln!("STATS phrase_terms_with_pos={} total_pos_entries={} ({} MB as u32)", counts.len(), total, total * 4 / 1_000_000);
        for (t, c) in counts.iter().take(15) {
            eprintln!("STATS term#{t} count={c}");
        }
        for cap in [100u32, 500, 1000, 2000, 5000, 10000, 50000] {
            let kept: u64 = counts.iter().map(|&(_, c)| (c as u64).min(cap as u64)).sum();
            let rare_terms = counts.iter().filter(|&&(_, c)| c <= cap).count();
            eprintln!("STATS cap={cap}: kept_entries={kept} ({} MB) terms_under_cap={rare_terms}/{}", kept * 4 / 1_000_000, counts.len());
        }
        eprintln!("STATS docs_entries={} ({} MB)", d_total, d_total * 4 / 1_000_000);
    }
    Index { docs, d_spans, ents, e_spans, ovf, o_spans }
}

// Boolean merges over sorted doc-id slices.
fn intersect(a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    out.clear();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        let (x, y) = (a[i], b[j]);
        if x == y {
            out.push(x);
            i += 1;
            j += 1;
        } else if x < y {
            i += 1;
        } else {
            j += 1;
        }
    }
}

fn union(a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    out.clear();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        let (x, y) = (a[i], b[j]);
        if x < y {
            out.push(x);
            i += 1;
        } else if y < x {
            out.push(y);
            j += 1;
        } else {
            out.push(x);
            i += 1;
            j += 1;
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
}

fn difference(a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    out.clear();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        let (x, y) = (a[i], b[j]);
        if x == y {
            i += 1;
            j += 1;
        } else if x < y {
            out.push(x);
            i += 1;
        } else {
            j += 1;
        }
    }
    out.extend_from_slice(&a[i..]);
}

// Evaluation.
fn write_u64(out: &mut Vec<u8>, mut v: u64) {
    let mut b = [0u8; 20];
    let mut i = 20;
    loop {
        i -= 1;
        b[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out.extend_from_slice(&b[i..]);
}

fn emit(out: &mut Vec<u8>, docs: &[u32]) {
    write_u64(out, docs.len() as u64);
    out.push(b' ');
    let mut sum: u64 = 0;
    for &d in docs {
        sum += d as u64;
    }
    write_u64(out, sum);
    for &d in docs.iter().take(10) {
        out.push(b' ');
        write_u64(out, d as u64);
    }
    out.push(b'\n');
}

/// True if the phrase anchored at (d, p) for term order[0] extends through
/// every other term at its expected offset.
fn phrase_hit(idx: &Index, terms: &[u32], order: &[usize], a: usize, d: u32, p: i64) -> bool {
    for &j in &order[1..] {
        let tp = p + (j as i64 - a as i64);
        if tp < 0 {
            return false;
        }
        let ok = if (tp as u32) < POS_LIMIT {
            idx.ents(terms[j]).binary_search(&((d << POS_SHIFT) | tp as u32)).is_ok()
        } else {
            idx.ovf(terms[j]).binary_search(&(((d as u64) << 32) | tp as u64)).is_ok()
        };
        if !ok {
            return false;
        }
    }
    true
}

fn eval_queries(idx: &Index, qs: &[Query]) -> Vec<u8> {
    let mut out = Vec::with_capacity(qs.len() * 30);
    let mut cur: Vec<u32> = Vec::new();
    let mut tmp: Vec<u32> = Vec::new();
    let mut matched: Vec<u32> = Vec::new();
    for q in qs {
        match q {
            Query::Bool { terms, ops } => {
                cur.clear();
                cur.extend_from_slice(idx.docs(terms[0]));
                for (op, &t) in ops.iter().zip(&terms[1..]) {
                    let d = idx.docs(t);
                    match *op {
                        OP_AND => intersect(&cur, d, &mut tmp),
                        OP_OR => union(&cur, d, &mut tmp),
                        _ => difference(&cur, d, &mut tmp),
                    }
                    std::mem::swap(&mut cur, &mut tmp);
                }
                emit(&mut out, &cur);
            }
            Query::Phrase { terms } => {
                matched.clear();
                // Anchor on the rarest term; check the rest in rising rarity.
                let k = terms.len();
                let mut order: [usize; 8] = [0; 8];
                for j in 0..k {
                    order[j] = j;
                }
                order[..k].sort_by_key(|&j| idx.ents(terms[j]).len());
                let a = order[0];
                for &e in idx.ents(terms[a]) {
                    let d = e >> POS_SHIFT;
                    if phrase_hit(idx, terms, &order[..k], a, d, (e & POS_MASK) as i64) {
                        matched.push(d);
                    }
                }
                for &e in idx.ovf(terms[a]) {
                    let d = (e >> 32) as u32;
                    if phrase_hit(idx, terms, &order[..k], a, d, (e & 0xFFFF_FFFF) as i64) {
                        matched.push(d);
                    }
                }
                matched.sort_unstable();
                matched.dedup();
                emit(&mut out, &matched);
            }
        }
    }
    out
}

/// Worker count: the container may see the host's core count while the
/// cgroup CPU quota allows far fewer, so read the quota when present.
fn worker_threads() -> usize {
    let host = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    if let Ok(s) = fs::read_to_string("/sys/fs/cgroup/cpu.max") {
        let mut it = s.split_whitespace();
        if let (Some(q), Some(p)) = (it.next(), it.next()) {
            if q != "max" {
                if let (Ok(q), Ok(p)) = (q.parse::<u64>(), p.parse::<u64>()) {
                    if q > 0 && p > 0 {
                        return (((q + p - 1) / p) as usize).max(1).min(host).min(32);
                    }
                }
            }
        }
    }
    if let (Ok(q), Ok(p)) = (
        fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us"),
        fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us"),
    ) {
        if let (Ok(q), Ok(p)) = (q.trim().parse::<i64>(), p.trim().parse::<u64>()) {
            if q > 0 && p > 0 {
                return (((q as u64 + p - 1) / p) as usize).max(1).min(host).min(32);
            }
        }
    }
    host.min(16)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = args.get(1).map(String::as_str).unwrap_or("/data");
    let out_dir = args.get(2).map(String::as_str).unwrap_or("/out");
    let docs_path = format!("{data_dir}/docs.txt");

    let t0 = std::time::Instant::now();
    let qdata = fs::read(format!("{data_dir}/queries.txt")).expect("read queries.txt");
    let mut terms = Terms::new(1 << 16);
    let queries = parse_queries(&qdata, &mut terms);
    drop(qdata);
    let n_terms = terms.len();
    let t_queries = t0.elapsed();

    let file_len = fs::metadata(&docs_path).expect("stat docs.txt").len() as usize;
    let nthreads = worker_threads();
    let ranges = chunk_ranges(file_len, &docs_path, nthreads.max(1));

    let mut parts: Vec<Post> = Vec::with_capacity(ranges.len());
    std::thread::scope(|s| {
        let tref = &terms;
        let handles: Vec<_> = ranges
            .iter()
            .map(|&(st, en)| {
                let path = &docs_path;
                s.spawn(move || thread_scan(path, st, en, tref, n_terms))
            })
            .collect();
        for h in handles {
            parts.push(h.join().expect("scan thread"));
        }
    });

    let t_scan = t0.elapsed();
    let index = build_index(&mut parts, n_terms);
    drop(parts);
    let t_merge = t0.elapsed();

    let mut pieces: Vec<Vec<u8>> = Vec::new();
    std::thread::scope(|s| {
        let nchunks = nthreads.max(1).min(queries.len().max(1));
        let size = queries.len() / nchunks + 1;
        let iref = &index;
        let handles: Vec<_> = queries
            .chunks(size)
            .map(|qc| s.spawn(move || eval_queries(iref, qc)))
            .collect();
        for h in handles {
            pieces.push(h.join().expect("eval thread"));
        }
    });

    let t_eval = t0.elapsed();
    if std::env::var_os("TRACKB_STATS").is_some() {
        eprintln!("STATS threads={} queries_setup={:?} scan={:?} merge={:?} eval={:?}",
            nthreads, t_queries, t_scan - t_queries, t_merge - t_scan, t_eval - t_merge);
    }
    fs::create_dir_all(out_dir).ok();
    let mut f = fs::File::create(format!("{out_dir}/results.txt")).expect("create results.txt");
    for p in &pieces {
        f.write_all(p).expect("write results.txt");
    }
}

