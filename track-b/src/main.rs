//! Speed Demon 2026, Track B (Index).
//!
//! Query-driven inverted index, std-only, no dependencies:
//!   1. Parse queries.txt first and collect every distinct term.
//!   2. One pass over docs.txt (parallel chunks): a byte-exact tokenizer per
//!      the spec records (doc, position) pairs only for query terms.
//!   3. BOOL queries merge sorted doc-id lists strictly left to right.
//!      PHRASE queries iterate the rarest term's occurrences and binary-search
//!      the other terms' position lists for the expected offsets.
//!
//! Reads /data/docs.txt and /data/queries.txt, writes /out/results.txt.

use std::fs;
use std::io::Write;

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

#[inline]
fn hash_bytes(b: &[u8]) -> u64 {
    // fxhash-style; deterministic across platforms, no libm anywhere.
    let mut h = b.len() as u64;
    let mut chunks = b.chunks_exact(8);
    for c in &mut chunks {
        h = (h.rotate_left(5) ^ u64::from_le_bytes(c.try_into().unwrap()))
            .wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    let mut tail = 0u64;
    let mut s = 0u32;
    for &x in chunks.remainder() {
        tail |= (x as u64) << s;
        s += 8;
    }
    h = (h.rotate_left(5) ^ tail).wrapping_mul(0x517c_c1b7_2722_0a95);
    if h == 0 {
        1 // 0 is the empty-slot sentinel
    } else {
        h
    }
}

// ---------------------------------------------------------------------------
// Term table: open addressing, hash + full byte comparison on hit.
// ---------------------------------------------------------------------------
struct Terms {
    slots: Vec<u64>, // hash, 0 = empty
    ids: Vec<u32>,   // term id per slot
    arena: Vec<u8>,
    spans: Vec<(u32, u32)>, // (offset, len) into arena, per term id
    used: usize,
}

impl Terms {
    fn new(min_cap: usize) -> Self {
        let cap = (min_cap * 2).next_power_of_two();
        Terms {
            slots: vec![0; cap],
            ids: vec![0; cap],
            arena: Vec::new(),
            spans: Vec::new(),
            used: 0,
        }
    }
    #[inline(always)]
    fn mask(&self) -> usize {
        self.slots.len() - 1
    }
    #[inline(always)]
    fn bytes(&self, id: u32) -> &[u8] {
        let (off, len) = self.spans[id as usize];
        &self.arena[off as usize..(off + len) as usize]
    }
    #[inline]
    fn get(&self, hash: u64, bytes: &[u8]) -> Option<u32> {
        let mut i = (hash as usize) & self.mask();
        loop {
            let h = self.slots[i];
            if h == 0 {
                return None;
            }
            if h == hash && self.bytes(self.ids[i]) == bytes {
                return Some(self.ids[i]);
            }
            i = (i + 1) & self.mask();
        }
    }
    fn insert(&mut self, hash: u64, bytes: &[u8]) -> u32 {
        if (self.used + 1) * 2 > self.slots.len() {
            self.grow();
        }
        let mut i = (hash as usize) & self.mask();
        loop {
            let h = self.slots[i];
            if h == 0 {
                let id = self.spans.len() as u32;
                self.slots[i] = hash;
                self.ids[i] = id;
                self.spans.push((self.arena.len() as u32, bytes.len() as u32));
                self.arena.extend_from_slice(bytes);
                self.used += 1;
                return id;
            }
            if h == hash && self.bytes(self.ids[i]) == bytes {
                return self.ids[i];
            }
            i = (i + 1) & self.mask();
        }
    }
    fn grow(&mut self) {
        let ncap = self.slots.len() * 2;
        let mut ns = vec![0u64; ncap];
        let mut ni = vec![0u32; ncap];
        let mask = ncap - 1;
        for i in 0..self.slots.len() {
            let h = self.slots[i];
            if h != 0 {
                let mut j = (h as usize) & mask;
                while ns[j] != 0 {
                    j = (j + 1) & mask;
                }
                ns[j] = h;
                ni[j] = self.ids[i];
            }
        }
        self.slots = ns;
        self.ids = ni;
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
// Corpus scan: for every query term, a list of (doc_id << 32) | position,
// appended in increasing (doc, position) order.
// ---------------------------------------------------------------------------
fn scan_chunk(data: &[u8], terms: &Terms, postings: &mut [Vec<u64>]) {
    let mut tb = [0u8; MAX_TERM + 8];
    let n = data.len();
    let mut i = 0usize;
    while i < n {
        // line: decimal id, TAB, text, LF
        let mut id: u32 = 0;
        while data[i] != b'\t' {
            id = id * 10 + (data[i] - b'0') as u32;
            i += 1;
        }
        i += 1;
        let mut pos: u32 = 0;
        while i < n && data[i] != b'\n' {
            let f = FOLD[data[i] as usize];
            if f == SEP {
                i += 1;
                continue;
            }
            // token run starts here
            let mut tl = 0usize;
            while i < n {
                let f = FOLD[data[i] as usize];
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
                let b = &tb[..tl];
                if let Some(tid) = terms.get(hash_bytes(b), b) {
                    postings[tid as usize].push(((id as u64) << 32) | pos as u64);
                }
            }
            pos += 1;
        }
        i += 1; // skip LF (or fall off the end)
    }
}

fn chunk_ranges(data: &[u8], nthreads: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::with_capacity(nthreads);
    let size = data.len() / nthreads + 1;
    let mut start = 0usize;
    while start < data.len() {
        let mut end = (start + size).min(data.len());
        while end < data.len() && data[end - 1] != b'\n' {
            end += 1;
        }
        ranges.push((start, end));
        start = end;
    }
    ranges
}

// ---------------------------------------------------------------------------
// Merged index: flat arenas, spans per term.
// ---------------------------------------------------------------------------
struct Index {
    entries: Vec<u64>,        // (doc<<32)|pos, sorted, per term
    entry_spans: Vec<(u32, u32)>,
    docs: Vec<u32>,           // deduplicated doc ids per term, ascending
    doc_spans: Vec<(u32, u32)>,
}

impl Index {
    #[inline(always)]
    fn entries(&self, tid: u32) -> &[u64] {
        let (off, len) = self.entry_spans[tid as usize];
        &self.entries[off as usize..(off + len) as usize]
    }
    #[inline(always)]
    fn docs(&self, tid: u32) -> &[u32] {
        let (off, len) = self.doc_spans[tid as usize];
        &self.docs[off as usize..(off + len) as usize]
    }
}

fn build_index(parts: &mut [Vec<Vec<u64>>], n_terms: usize) -> Index {
    let mut entry_spans = vec![(0u32, 0u32); n_terms];
    let mut doc_spans = vec![(0u32, 0u32); n_terms];
    let total: usize = parts.iter().map(|p| p.iter().map(Vec::len).sum::<usize>()).sum();
    let mut entries: Vec<u64> = Vec::with_capacity(total);
    let mut docs: Vec<u32> = Vec::with_capacity(total / 2 + 16);
    for tid in 0..n_terms {
        let start = entries.len() as u32;
        let dstart = docs.len() as u32;
        let mut last = u32::MAX;
        for p in parts.iter_mut() {
            let v = std::mem::take(&mut p[tid]);
            for &x in &v {
                let d = (x >> 32) as u32;
                if d != last {
                    docs.push(d);
                    last = d;
                }
            }
            entries.extend_from_slice(&v);
        }
        entry_spans[tid] = (start, entries.len() as u32 - start);
        doc_spans[tid] = (dstart, docs.len() as u32 - dstart);
    }
    Index { entries, entry_spans, docs, doc_spans }
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
                order[..k].sort_by_key(|&j| idx.entries(terms[j]).len());
                let a = order[0];
                let anchor = idx.entries(terms[a]);
                'next: for &e in anchor {
                    let d = (e >> 32) as u32;
                    let p = (e & 0xFFFF_FFFF) as i64;
                    for &j in &order[1..k] {
                        let tp = p + (j as i64 - a as i64);
                        if tp < 0 || tp > u32::MAX as i64 {
                            continue 'next;
                        }
                        let target = ((d as u64) << 32) | (tp as u64);
                        if idx.entries(terms[j]).binary_search(&target).is_err() {
                            continue 'next;
                        }
                    }
                    if d != matched.last().copied().unwrap_or(u32::MAX) {
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

    let qdata = fs::read(format!("{data_dir}/queries.txt")).expect("read queries.txt");
    let mut terms = Terms::new(1 << 16);
    let queries = parse_queries(&qdata, &mut terms);
    drop(qdata);
    let n_terms = terms.len();

    let ddata = fs::read(format!("{data_dir}/docs.txt")).expect("read docs.txt");
    let nthreads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let ranges = chunk_ranges(&ddata, nthreads.max(1));

    let mut parts: Vec<Vec<Vec<u64>>> = Vec::with_capacity(ranges.len());
    std::thread::scope(|s| {
        let tref = &terms;
        let handles: Vec<_> = ranges
            .iter()
            .map(|&(st, en)| {
                let data = &ddata[st..en];
                s.spawn(move || {
                    let mut p: Vec<Vec<u64>> = (0..n_terms).map(|_| Vec::new()).collect();
                    scan_chunk(data, tref, &mut p);
                    p
                })
            })
            .collect();
        for h in handles {
            parts.push(h.join().expect("scan thread"));
        }
    });
    drop(ddata);

    let index = build_index(&mut parts, n_terms);
    drop(parts);

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

    fs::create_dir_all(out_dir).ok();
    let mut f = fs::File::create(format!("{out_dir}/results.txt")).expect("create results.txt");
    for p in &pieces {
        f.write_all(p).expect("write results.txt");
    }
}
