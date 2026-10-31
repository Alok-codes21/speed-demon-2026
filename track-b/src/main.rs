//! Speed Demon 2026, Track B (Index).
//!
//! Query-driven inverted index, std-only, no dependencies:
//!   1. Parse queries.txt first and collect every distinct term.
//!   2. Stream docs.txt in blocks (parallel ranges): a byte-exact tokenizer
//!      per the spec records deduplicated doc ids for query terms and the
//!      file offset of every doc line.
//!   3. BOOL queries merge sorted doc-id lists strictly left to right.
//!      PHRASE queries intersect their terms' doc lists, then re-tokenize
//!      each candidate doc straight from the file to verify adjacency.
//!      No positional index is ever stored.
//!
//! Reads /data/docs.txt and /data/queries.txt, writes /out/results.txt.

use std::fs;
use std::io::Write;
use std::os::unix::fs::FileExt;

// ---------------------------------------------------------------------------
// Tokenizer: fold A-Z to a-z only; a token is a maximal run of [a-z0-9].
// Every other byte (including all bytes >= 0x80) is a separator.
// ---------------------------------------------------------------------------
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

// fxhash-style rolling hash; deterministic across platforms, no libm.
const HMUL: u64 = 0x517c_c1b7_2722_0a95;

struct Hasher {
    h: u64,
    block: u64,
    shift: u32,
}

impl Hasher {
    #[inline(always)]
    fn new() -> Self {
        Hasher { h: 0, block: 0, shift: 0 }
    }
    #[inline(always)]
    fn byte(&mut self, b: u8) {
        self.block |= (b as u64) << self.shift;
        self.shift += 8;
        if self.shift == 64 {
            self.h = (self.h.rotate_left(5) ^ self.block).wrapping_mul(HMUL);
            self.block = 0;
            self.shift = 0;
        }
    }
    #[inline(always)]
    fn finish(self, len: u64) -> u64 {
        let mut h = (self.h.rotate_left(5) ^ self.block).wrapping_mul(HMUL);
        h = (h.rotate_left(5) ^ len).wrapping_mul(HMUL);
        if h == 0 {
            1 // reserve 0
        } else {
            h
        }
    }
}

#[inline(always)]
fn hash_bytes(b: &[u8]) -> u64 {
    let mut hs = Hasher::new();
    for &x in b {
        hs.byte(x);
    }
    hs.finish(b.len() as u64)
}

// ---------------------------------------------------------------------------
// Term table: open addressing over single u64 entries
//   [ hash32 : low 32 | term id : high 32 ]
// so a probe touches one cache line before any byte comparison.
// A 4M-bit summary filter lets the corpus scan skip nearly every miss
// without touching the table.
// ---------------------------------------------------------------------------
const FILTER_BITS: usize = 1 << 22;
const EMPTY: u64 = u64::MAX;

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
    /// Full lookup; returns the term id.
    #[inline(always)]
    fn get(&self, hash: u64, bytes: &[u8]) -> Option<u32> {
        let h32 = Self::h32(hash);
        let mut i = (h32 as usize) & self.mask();
        loop {
            let e = self.slots[i];
            if e == EMPTY {
                return None;
            }
            let id = (e >> 32) as u32;
            if e as u32 == h32 && self.bytes(id) == bytes {
                return Some(id);
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
            if e as u32 == h32 && self.bytes((e >> 32) as u32) == bytes {
                return (e >> 32) as u32;
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

// ---------------------------------------------------------------------------
// Queries.
// ---------------------------------------------------------------------------
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
                    let h = hash_bytes(w);
                    t.push(terms.insert(h, w));
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
                let h = hash_bytes(w);
                t.push(terms.insert(h, w));
            }
            qs.push(Query::Phrase { terms: t });
        } else {
            panic!("bad query line");
        }
    }
    qs
}

// ---------------------------------------------------------------------------
// Corpus scan (streaming). Per thread: deduplicated ascending doc ids per
// term, plus (doc id, line offset) for every doc seen.
// A tiny per-thread cache of recently touched (term, doc) pairs keeps hot
// vocabulary terms off the posting vectors.
// ---------------------------------------------------------------------------
struct Post {
    docs: Vec<Vec<u32>>,
    offs: Vec<(u32, u64)>,
}

const RECENT: usize = 4096;

/// Scans whole lines in `data` (whose first byte sits at `base` in the
/// file): decimal id, TAB, text, LF.
fn scan_lines(data: &[u8], base: u64, terms: &Terms, post: &mut Post, recent: &mut [(u32, u32)]) {
    let mut tb = [0u8; MAX_TERM + 8];
    let n = data.len();
    let mut i = 0usize;
    while i < n {
        let line_start = i;
        let mut id: u32 = 0;
        while data[i] != b'\t' {
            id = id * 10 + (data[i] - b'0') as u32;
            i += 1;
        }
        i += 1;
        post.offs.push((id, base + line_start as u64));
        while i < n && data[i] != b'\n' {
            let f = FOLD[data[i] as usize];
            if f == SEP {
                i += 1;
                continue;
            }
            let mut hs = Hasher::new();
            let mut tl = 0usize;
            while i < n {
                let f = FOLD[data[i] as usize];
                if f == SEP {
                    break;
                }
                if tl < tb.len() {
                    tb[tl] = f;
                }
                hs.byte(f);
                tl += 1;
                i += 1;
            }
            if tl <= MAX_TERM {
                let h = hs.finish(tl as u64);
                if terms.filter_test(h) {
                    if let Some(t) = terms.get(h, &tb[..tl]) {
                        let rc = &mut recent[(t as usize) & (RECENT - 1)];
                        if *rc != (t, id) {
                            if post.docs[t as usize].last() != Some(&id) {
                                post.docs[t as usize].push(id);
                            }
                            *rc = (t, id);
                        }
                    }
                }
            }
        }
        i += 1; // skip LF
    }
}

const BLOCK: usize = 1 << 21; // 2 MB streaming blocks

/// Streams [start, end) of docs.txt; both bounds sit right after an LF.
fn thread_scan(path: &str, start: usize, end: usize, terms: &Terms, n_terms: usize) -> Post {
    let mut post = Post {
        docs: (0..n_terms).map(|_| Vec::new()).collect(),
        offs: Vec::new(),
    };
    let mut recent = vec![(u32::MAX, 0u32); RECENT];
    let f = fs::File::open(path).expect("open docs.txt");
    let mut buf = vec![0u8; BLOCK + (1 << 16)];
    let mut carry = 0usize; // leading partial line in buf
    let mut pos = start;
    while pos < end {
        let want = BLOCK.min(end - pos);
        let n = f.read_at(&mut buf[carry..carry + want], pos as u64).expect("read docs.txt");
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
            scan_lines(&buf[..cut], (start + (pos - start) - avail) as u64, terms, &mut post, &mut recent);
            buf.copy_within(cut..avail, 0);
            carry = avail - cut;
        }
    }
    debug_assert!(carry == 0);
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

// ---------------------------------------------------------------------------
// Merged index: one flat doc-id arena with per-term spans, plus the
// doc id -> line offset table used to re-read docs for phrase checks.
// ---------------------------------------------------------------------------
struct Index {
    docs: Vec<u32>,
    d_spans: Vec<(u32, u32)>,
    offsets: Vec<u64>,
}

impl Index {
    #[inline(always)]
    fn docs(&self, tid: u32) -> &[u32] {
        let (off, len) = self.d_spans[tid as usize];
        &self.docs[off as usize..(off + len) as usize]
    }
}

fn build_index(parts: &mut Vec<Post>, n_terms: usize) -> Index {
    let mut d_spans = vec![(0u32, 0u32); n_terms];
    let d_total: usize = parts.iter().map(|p| p.docs.iter().map(Vec::len).sum::<usize>()).sum();
    let mut docs: Vec<u32> = Vec::with_capacity(d_total);
    for tid in 0..n_terms {
        let ds = docs.len() as u32;
        for p in parts.iter_mut() {
            docs.extend_from_slice(&std::mem::take(&mut p.docs[tid]));
        }
        d_spans[tid] = (ds, docs.len() as u32 - ds);
    }
    let max_id = parts
        .iter()
        .flat_map(|p| p.offs.iter().map(|&(id, _)| id))
        .max()
        .unwrap_or(0);
    let mut offsets = vec![u64::MAX; max_id as usize + 1];
    for p in parts.iter() {
        for &(id, off) in &p.offs {
            offsets[id as usize] = off;
        }
    }
    Index { docs, d_spans, offsets }
}

// ---------------------------------------------------------------------------
// Boolean merges over sorted doc-id slices.
// ---------------------------------------------------------------------------
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

// ---------------------------------------------------------------------------
// Evaluation.
// ---------------------------------------------------------------------------
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

/// Re-tokenizes doc `d` from the file and checks whether the phrase terms
/// occur at consecutive token positions. Positions count every token run,
/// exactly as the corpus scan does. `buf` and `hits` are caller-owned
/// scratch, reused across candidates.
fn phrase_doc_match(
    f: &fs::File,
    offsets: &[u64],
    phrase_words: &[&[u8]],
    d: u32,
    buf: &mut [u8],
    hits: &mut Vec<(u8, u32)>,
) -> bool {
    let off = offsets[d as usize];
    if off == u64::MAX {
        return false;
    }
    let n = f.read_at(buf, off).unwrap_or(0);
    let mut line = &buf[..n];
    if let Some(p) = line.iter().position(|&b| b == b'\n') {
        line = &line[..p];
    }
    let text = match line.iter().position(|&b| b == b'\t') {
        Some(p) => &line[p + 1..],
        None => return false,
    };
    let k = phrase_words.len();
    hits.clear();
    let mut tb = [0u8; MAX_TERM + 8];
    let n = text.len();
    let mut i = 0usize;
    let mut pos: u32 = 0;
    while i < n {
        let f = FOLD[text[i] as usize];
        if f == SEP {
            i += 1;
            continue;
        }
        let mut tl = 0usize;
        while i < n {
            let f = FOLD[text[i] as usize];
            if f == SEP {
                break;
            }
            if tl < tb.len() {
                tb[tl] = f;
            }
            tl += 1;
            i += 1;
        }
        if tl <= MAX_TERM {
            let tok = &tb[..tl];
            for (j, w) in phrase_words.iter().enumerate() {
                if w.len() == tl && *w == tok {
                    hits.push((j as u8, pos));
                }
            }
        }
        pos += 1;
    }
    let mut found = false;
    for &(j0, p0) in hits.iter() {
        if j0 != 0 {
            continue;
        }
        if (1..k).all(|j| hits.iter().any(|&(jj, pp)| jj as usize == j && pp == p0 + j as u32)) {
            found = true;
            break;
        }
    }
    found
}

fn eval_queries(idx: &Index, terms: &Terms, file: &fs::File, qs: &[Query]) -> Vec<u8> {
    let mut out = Vec::with_capacity(qs.len() * 30);
    let mut cur: Vec<u32> = Vec::new();
    let mut tmp: Vec<u32> = Vec::new();
    let mut matched: Vec<u32> = Vec::new();
    let mut docbuf = vec![0u8; 1 << 14];
    let mut hits: Vec<(u8, u32)> = Vec::with_capacity(64);
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
            Query::Phrase { terms: pterms } => {
                matched.clear();
                let pw: Vec<&[u8]> = pterms.iter().map(|&t| terms.bytes(t)).collect();
                // Anchor on the term with the shortest doc list; the other
                // lists are probed by binary search for each candidate doc.
                let k = pterms.len();
                let mut order: [usize; 8] = [0; 8];
                for j in 0..k {
                    order[j] = j;
                }
                order[..k].sort_by_key(|&j| idx.docs(pterms[j]).len());
                let a = order[0];
                'cand: for &d in idx.docs(pterms[a]) {
                    for &j in &order[1..k] {
                        if idx.docs(pterms[j]).binary_search(&d).is_err() {
                            continue 'cand;
                        }
                    }
                    if phrase_doc_match(file, &idx.offsets, &pw, d, &mut docbuf, &mut hits) {
                        matched.push(d);
                    }
                }
                emit(&mut out, &matched);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = args.get(1).map(String::as_str).unwrap_or("/data");
    let out_dir = args.get(2).map(String::as_str).unwrap_or("/out");
    let docs_path = format!("{data_dir}/docs.txt");

    let qdata = fs::read(format!("{data_dir}/queries.txt")).expect("read queries.txt");
    let mut terms = Terms::new(1 << 16);
    let queries = parse_queries(&qdata, &mut terms);
    drop(qdata);
    let n_terms = terms.len();

    let file_len = fs::metadata(&docs_path).expect("stat docs.txt").len() as usize;
    let nthreads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
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

    let index = build_index(&mut parts, n_terms);
    drop(parts);

    let docs_file = fs::File::open(&docs_path).expect("open docs.txt");
    let mut pieces: Vec<Vec<u8>> = Vec::new();
    std::thread::scope(|s| {
        let nchunks = nthreads.max(1).min(queries.len().max(1));
        let size = queries.len() / nchunks + 1;
        let iref = &index;
        let tref = &terms;
        let fref = &docs_file;
        let handles: Vec<_> = queries
            .chunks(size)
            .map(|qc| s.spawn(move || eval_queries(iref, tref, fref, qc)))
            .collect();
        for h in handles {
            pieces.push(h.join().expect("eval thread"));
        }
    });

    fs::create_dir_all(out_dir).ok();
    let mut f = fs::File::create(format!("{out_dir}/results.txt")).expect("create results.txt");
    for p in &pieces {
        f.write_all(p).expect("write results.txt");
    }
}
