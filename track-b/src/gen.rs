//! PGO training corpus: synthetic docs + queries with the published
//! generator's shape (token lengths, query mix, skew), from xorshift only.
use std::fmt::Write as _;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        self.0 = x; x
    }
    fn below(&mut self, n: usize) -> usize { (self.next() % n as u64) as usize }
    // Skewed vocabulary index: small ranks dominate, like the real corpus.
    fn pick(&mut self, n: usize) -> usize {
        let u = self.next() as f64 / u64::MAX as f64;
        ((n as f64) * u * u * u) as usize
    }
}

const LENS: &str = "12222222233333333334444444555555666666777777778888888999999";

fn main() {
    let seed: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let (ndocs, nq, nvocab) = (12000usize, 3000usize, 60000usize);
    let mut r = Rng(0x9E3779B97F4A7C15 ^ seed);
    let vocab: Vec<String> = (0..nvocab).map(|_| {
        let n = LENS.as_bytes()[r.below(LENS.len())] - b'0';
        (0..n).map(|_| (b'a' + r.below(26) as u8) as char).collect()
    }).collect();
    let mut docs = String::with_capacity(ndocs * 110);
    for d in 0..ndocs {
        let mut line = format!("{d}\t");
        for _ in 0..90 + r.below(40) { line.push_str(&vocab[r.pick(nvocab)]); line.push(' '); }
        line.pop(); line.push('\n');
        docs.push_str(&line);
    }
    std::fs::write("/data/docs.txt", &docs).unwrap();
    let dl: Vec<&[u8]> = docs.lines().map(|l| l.as_bytes()).collect();
    let mut q = String::new();
    for _ in 0..nq {
        if r.below(8) < 5 {
            let n = 1 + r.pick(6);
            let mut s = String::from("BOOL");
            for k in 0..n {
                if k > 0 { s.push(' '); s.push_str(["AND", "OR", "NOT"][r.below(3)]); }
                s.push(' '); s.push_str(&vocab[r.pick(3000)]); // head: matches exist
            }
            q.push_str(&s); q.push('\n');
        } else {
            // Phrase: consecutive tokens of a random doc, so matches exist.
            let b = dl[r.below(dl.len())];
            let toks: Vec<&[u8]> = b.split(|&c| c == b' ').skip(1).collect();
            let at = r.below(toks.len().saturating_sub(6).max(1));
            let n = 2 + r.below(4);
            let _ = write!(q, "PHRASE");
            for t in &toks[at..(at + n).min(toks.len())] { q.push(' '); q.push_str(std::str::from_utf8(t).unwrap()); }
            q.push('\n');
        }
    }
    std::fs::write("/data/queries.txt", q).unwrap();
}
