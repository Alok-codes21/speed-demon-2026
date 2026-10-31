use std::fs::File; use std::os::unix::io::AsRawFd; use std::sync::atomic::{AtomicUsize, Ordering}; use std::sync::Barrier; use std::thread; const HOURS: usize = 72; const SLOTS: usize = 4096;
const MAX_NAMES: usize = 2048; const CHUNK_WORDS: usize = 1024; const CHUNK_CAP: usize = CHUNK_WORDS - 2; const ID_MASK: u32 = 0x7ff; const ERR_BIT: u32 = 0x800; const PACK_MAX: u32 = 1 << 20;
const LABEL: usize = 21; const PROT_READ: i32 = 1; const PROT_WRITE: i32 = 2; const MAP_SHARED: i32 = 0x01; const MAP_PRIVATE: i32 = 0x02; const MAP_FIXED: i32 = 0x10; const MAP_ANON: i32 = 0x20;
const MADV_DONTNEED: i32 = 4; const PR_SET_THP_DISABLE: i32 = 41; extern "C" { fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32; fn ftruncate(fd: i32, len: i64) -> i32; fn prctl(opt: i32, a2: usize, a3: usize, a4: usize, a5: usize) -> i32; } #[derive(Clone, Copy)]
struct Name { ptr: *const u8, len: u32, } impl Name { #[inline(always)] fn bytes(self) -> &'static [u8] { unsafe { std::slice::from_raw_parts(self.ptr, self.len as usize) } } } #[inline(always)]
fn name_eq<const SIMD: bool>(a: Name, b: Name) -> bool { if a.len != b.len { return false; } let n = a.len as usize; #[cfg(target_arch = "x86_64")] if SIMD && n <= 32 { unsafe {
use std::arch::x86_64::*; let x = _mm256_loadu_si256(a.ptr as *const __m256i); let y = _mm256_loadu_si256(b.ptr as *const __m256i); let eq = _mm256_movemask_epi8(_mm256_cmpeq_epi8(x, y)) as u32;
let mask = if n == 32 { u32::MAX } else { (1u32 << n) - 1 }; return !eq & mask == 0; } } a.bytes() == b.bytes() } struct Interner { names: Vec<Name>, slots: Vec<u16>, } impl Interner {
fn new() -> Self { Interner { names: Vec::with_capacity(MAX_NAMES), slots: vec![0u16; SLOTS] } } #[inline(always)] fn hash(s: Name) -> usize { let n = s.len as usize; unsafe {
let mut a = (s.ptr as *const u64).read_unaligned(); a = if n >= 8 { a } else { a & ((1u64 << (8 * n)) - 1) }; let b = if n >= 8 { (s.ptr.add(n - 8) as *const u64).read_unaligned() } else { 0 };
let mut h = (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15); h = (h ^ a).wrapping_mul(0xBF58_476D_1CE4_E5B9); h = (h ^ b).rotate_left(31).wrapping_mul(0x94D0_49BB_1331_11EB); (h ^ (h >> 29)) as usize }
} #[inline(always)] fn intern<const SIMD: bool>(&mut self, s: Name) -> u32 { let mut i = Self::hash(s) & (SLOTS - 1); loop { let id1 = unsafe { *self.slots.get_unchecked(i) }; if id1 == 0 {
let id = self.names.len(); self.names.push(s); self.slots[i] = id as u16 + 1; return id as u32; } if name_eq::<SIMD>(self.names[id1 as usize - 1], s) { return id1 as u32 - 1; }
i = (i + 1) & (SLOTS - 1); } } } #[inline(always)] unsafe fn find_byte<const SIMD: bool>(mut s: *const u8, c: u8) -> *const u8 { #[cfg(target_arch = "x86_64")] if SIMD { use std::arch::x86_64::*;
let v = _mm256_set1_epi8(c as i8); loop { let m = _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(s as *const __m256i), v)); if m != 0 { return s.add(m.trailing_zeros() as usize); }
s = s.add(32); } } while *s != c { s = s.add(1); } s } #[inline(always)] unsafe fn scan_line<const SIMD: bool>(m: *const u8) -> (*const u8, *const u8, *const u8) { #[cfg(target_arch = "x86_64")]
if SIMD { use std::arch::x86_64::*; let a = _mm256_loadu_si256(m as *const __m256i); let b = _mm256_loadu_si256(m.add(32) as *const __m256i);
let sp = (_mm256_movemask_epi8(_mm256_cmpeq_epi8(a, _mm256_set1_epi8(0x20))) as u32 as u64) | ((_mm256_movemask_epi8(_mm256_cmpeq_epi8(b, _mm256_set1_epi8(0x20))) as u32 as u64) << 32);
let nl = (_mm256_movemask_epi8(_mm256_cmpeq_epi8(a, _mm256_set1_epi8(0x0a))) as u32 as u64) | ((_mm256_movemask_epi8(_mm256_cmpeq_epi8(b, _mm256_set1_epi8(0x0a))) as u32 as u64) << 32); if nl != 0 {
let s1 = sp.trailing_zeros(); let s2 = (sp & (sp - 1)).trailing_zeros(); return (m.add(s1 as usize + 1), m.add(s2 as usize), m.add(nl.trailing_zeros() as usize)); }
let name = m.add(sp.trailing_zeros() as usize + 1); let name_end = find_byte::<SIMD>(name, b' '); return (name, name_end, find_byte::<SIMD>(name_end.add(5), b'\n')); } let mut s1 = 0usize;
while *m.add(s1) != b' ' { s1 += 1; } let mut s2 = s1 + 1; while *m.add(s2) != b' ' { s2 += 1; } let mut e = s2 + 1; while *m.add(e) != b'\n' { e += 1; } (m.add(s1 + 1), m.add(s2), m.add(e)) }
#[inline(always)] unsafe fn dec8(end: *const u8, len: usize) -> u32 { let w = (end.sub(8) as *const u64).read_unaligned().swap_bytes();
let w = if len == 8 { w } else { w & ((1u64 << (8 * len)) - 1) }; let v = w & 0x0F0F_0F0F_0F0F_0F0F;
let pairs = (v & 0x00FF_00FF_00FF_00FF).wrapping_add(((v >> 8) & 0x00FF_00FF_00FF_00FF).wrapping_mul(10));
let quads = (pairs & 0x0000_FFFF_0000_FFFF).wrapping_add(((pairs >> 16) & 0x0000_FFFF_0000_FFFF).wrapping_mul(100)); (quads & 0xFFFF_FFFF) as u32 + (quads >> 32) as u32 * 10_000 } #[inline(always)]
unsafe fn decode_latency(eol: *const u8, len: usize) -> u32 { if len <= 8 { dec8(eol, len) } else if len == 9 { (*eol.sub(9) - b'0') as u32 * 100_000_000 + dec8(eol, 8) } else {
((*eol.sub(10) - b'0') as u32 * 10 + (*eol.sub(9) - b'0') as u32) * 100_000_000 + dec8(eol, 8) } } struct Parser { local: Interner, arena: *mut u32, next_chunk: usize, heads: [u32; HOURS],
hour_of_chunk: Vec<u8>, released_upto: usize, wide: [Vec<u32>; HOURS], } impl Parser { fn new(range_bytes: usize) -> Parser { let chunks = (range_bytes / 40 + 2) / CHUNK_CAP + 2 + HOURS;
let arena = unsafe { mmap(std::ptr::null_mut(), chunks * CHUNK_WORDS * 4, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0) as *mut u32 }; assert!(arena as isize != -1, "arena mmap failed");
Parser { local: Interner::new(), arena, next_chunk: 0, heads: [0; HOURS], hour_of_chunk: Vec::new(), released_upto: 0, wide: std::array::from_fn(|_| Vec::new()), } } #[inline(always)]
unsafe fn run<const SIMD: bool>(&mut self, mut p: *const u8, end: *const u8) { while p < end { let day = (*p.add(8) - b'0') as usize * 10 + (*p.add(9) - b'0') as usize;
let hh = (*p.add(11) - b'0') as usize * 10 + (*p.add(12) - b'0') as usize; let h = (if *p.add(6) == b'0' { day - 30 } else { day + 1 }) * 24 + hh - 18;
let (name, name_end, eol) = scan_line::<SIMD>(p.add(28)); let id = self.local.intern::<SIMD>(Name { ptr: name, len: (name_end as usize - name as usize) as u32, });
let key = id | if *name_end.add(1) == b'5' { ERR_BIT } else { 0 }; let latency = decode_latency(eol, eol as usize - name_end as usize - 5); p = eol.add(1); if latency >= PACK_MAX {
let w = self.wide.get_unchecked_mut(h); w.push(latency); w.push(key); continue; } let head = self.heads.get_unchecked_mut(h); let mut ci = *head as usize;
if ci == 0 || *self.arena.add((ci - 1) * CHUNK_WORDS + 1) as usize == CHUNK_CAP { let nci = self.next_chunk; self.next_chunk += 1; self.hour_of_chunk.push(h as u8);
*self.arena.add(nci * CHUNK_WORDS) = *head; *self.arena.add(nci * CHUNK_WORDS + 1) = 0; ci = nci + 1; *head = ci as u32; } let base = (ci - 1) * CHUNK_WORDS;
let n = *self.arena.add(base + 1) as usize; *self.arena.add(base + 2 + n) = (latency << 12) | key; *self.arena.add(base + 1) = n as u32 + 1; } } #[target_feature(enable = "avx2")]
unsafe fn run_avx2(&mut self, p: *const u8, end: *const u8) { self.run::<true>(p, end) } fn release_prefix(&mut self, finished: &[std::sync::atomic::AtomicBool]) { let mut c = self.released_upto;
while c < self.next_chunk && finished[self.hour_of_chunk[c] as usize].load(Ordering::Acquire) { c += 1; } if c - self.released_upto >= 64 { unsafe { madvise(
self.arena.add(self.released_upto * CHUNK_WORDS) as *mut u8, (c - self.released_upto) * CHUNK_WORDS * 4, MADV_DONTNEED, ); } self.released_upto = c; } } #[inline(always)]
fn each(&self, h: usize, rank: &[u32], mut f: impl FnMut(u32, u32, u32)) { let mut ci = self.heads[h] as usize; while ci != 0 { let base = (ci - 1) * CHUNK_WORDS; unsafe {
let n = *self.arena.add(base + 1) as usize; for i in 0..n { let rec = *self.arena.add(base + 2 + i); f(rank[(rec & ID_MASK) as usize], rec & ERR_BIT, rec >> 12); } ci = *self.arena.add(base) as usize;
} } let w = &self.wide[h]; for pair in w.chunks_exact(2) { f(rank[(pair[1] & ID_MASK) as usize], pair[1] & ERR_BIT, pair[0]); } } } #[inline(always)] fn percentiles(a: &mut [u32]) -> (u32, u32, u32) {
let n = a.len(); let r50 = (50 * n + 99) / 100 - 1; let r95 = (95 * n + 99) / 100 - 1; let r99 = (99 * n + 99) / 100 - 1; if n <= 64 { a.sort_unstable(); return (a[r50], a[r95], a[r99]); }
let p99 = *a.select_nth_unstable(r99).1; let lo = &mut a[..r99]; let p95 = if r95 == r99 { p99 } else { *lo.select_nth_unstable(r95).1 }; let lo2 = &mut a[..r95];
let p50 = if r50 >= lo2.len() { p95 } else { *lo2.select_nth_unstable(r50).1 }; (p50, p95, p99) } #[inline(always)] fn width(v: u32) -> u64 { if v == 0 { 1 } else { (v.ilog10() + 1) as u64 } }
static DIGIT_PAIRS: [u8; 200] = *b"00010203040506070809101112131415161718192021222324252627282930313233343536373839404142434445464748495051525354555657585960616263646566676869707172737475767778798081828384858687888990919293949596979899";
#[inline(always)] unsafe fn write_u32(o: *mut u8, v: u32) -> *mut u8 { let mut buf = [0u8; 10]; let mut i = 10; let mut v = v; while v >= 100 { let d = (v % 100) as usize * 2; v /= 100; i -= 2;
buf[i] = DIGIT_PAIRS[d]; buf[i + 1] = DIGIT_PAIRS[d + 1]; } if v < 10 { i -= 1; buf[i] = b'0' + v as u8; } else { i -= 2; buf[i] = DIGIT_PAIRS[v as usize * 2];
buf[i + 1] = DIGIT_PAIRS[v as usize * 2 + 1]; } let n = 10 - i; std::ptr::copy_nonoverlapping(buf.as_ptr().add(i), o, n); o.add(n) } fn hour_label(h: usize) -> [u8; LABEL] { let t = h + 18;
let d = t / 24; let hh = (t % 24) as u8; let (mo, dy) = [(10u8, 30u8), (10, 31), (11, 1), (11, 2)][d]; let mut b = *b"2026-10-30T18:00:00Z "; b[6] = b'0' + mo % 10; b[8] = b'0' + dy / 10;
b[9] = b'0' + dy % 10; b[11] = b'0' + hh / 10; b[12] = b'0' + hh % 10; b } #[derive(Clone, Copy, Default)] struct Line { e: u32, n: u32, p50: u32, p95: u32, p99: u32, errors: u32, ppm: u32, }
struct Share<T>(std::cell::UnsafeCell<T>); unsafe impl<T> Send for Share<T> {} unsafe impl<T> Sync for Share<T> {} impl<T> Share<T> { fn get(&self) -> *mut T { self.0.get() } } fn main() { unsafe {
prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0); } let args: Vec<String> = std::env::args().collect(); let data_dir = args.get(1).map(String::as_str).unwrap_or("/data");
let out_dir = args.get(2).map(String::as_str).unwrap_or("/out"); let nt = std::env::var("SD_THREADS") .ok() .and_then(|v| v.parse().ok())
.unwrap_or_else(|| thread::available_parallelism().map(|x| x.get()).unwrap_or(1)) .clamp(1, 12); let input = File::open(format!("{data_dir}/requests.log")).expect("open requests.log");
let size = input.metadata().expect("stat").len() as usize; let log = unsafe { let region = mmap(std::ptr::null_mut(), size + 4096, PROT_READ, MAP_PRIVATE | MAP_ANON, -1, 0);
if region as isize == -1 { eprintln!("input anon mmap failed"); std::process::exit(1); }
if size > 0 && mmap(region, size, PROT_READ, MAP_PRIVATE | MAP_FIXED, input.as_raw_fd(), 0) as isize == -1 { eprintln!("input file mmap failed"); std::process::exit(1); } region };
let mut cut = vec![0usize; nt + 1]; for t in 1..nt { let mut c = (size * t / nt).max(cut[t - 1]); while c < size && unsafe { *log.add(c - 1) } != b'\n' { c += 1; } cut[t] = c; } cut[nt] = size;
let log_addr = log as usize; let parsers: Vec<Share<Parser>> = (0..nt).map(|t| Share(std::cell::UnsafeCell::new(Parser::new(cut[t + 1] - cut[t])))).collect();
let ranks: Vec<Share<Vec<u32>>> = (0..nt).map(|_| Share(std::cell::UnsafeCell::new(Vec::new()))).collect(); let gnames = Share(std::cell::UnsafeCell::new(Vec::<Name>::new()));
let hour_lines: Vec<Share<Vec<Line>>> = (0..HOURS).map(|_| Share(std::cell::UnsafeCell::new(Vec::new()))).collect();
let hour_off: Vec<Share<u64>> = (0..HOURS + 1).map(|_| Share(std::cell::UnsafeCell::new(0u64))).collect(); let text = AtomicUsize::new(0); let claim = AtomicUsize::new(0);
let claim_text = AtomicUsize::new(0); let finished: Vec<std::sync::atomic::AtomicBool> = (0..HOURS).map(|_| std::sync::atomic::AtomicBool::new(false)).collect();
let releasing = std::sync::atomic::AtomicBool::new(false); let bar = Barrier::new(nt); let simd = std::arch::is_x86_feature_detected!("avx2"); thread::scope(|s| { for t in 0..nt {
let parsers = &parsers; let ranks = &ranks; let gnames = &gnames; let hour_lines = &hour_lines; let hour_off = &hour_off; let text = &text; let claim = &claim; let claim_text = &claim_text;
let finished = &finished; let releasing = &releasing; let bar = &bar; let cut = &cut; s.spawn(move || { let log = log_addr as *const u8; let me = unsafe { &mut *parsers[t].get() };
let (p0, p1) = unsafe { (log.add(cut[t]), log.add(cut[t + 1])) }; if simd { unsafe { me.run_avx2(p0, p1) }; } else { unsafe { me.run::<false>(p0, p1) }; } bar.wait(); if t == 0 {
let mut all = Interner::new(); for (i, ps) in parsers.iter().enumerate() { let local = unsafe { &(*ps.get()).local }; let mut r = Vec::with_capacity(local.names.len()); for &n in &local.names {
r.push(all.intern::<false>(n)); } unsafe { *ranks[i].get() = r }; } let mut sorted = all.names.clone(); sorted.sort_unstable_by(|a, b| a.bytes().cmp(b.bytes()));
let mut rank_of = vec![0u32; all.names.len()]; for (rank, &n) in sorted.iter().enumerate() { let id = all.intern::<false>(n); rank_of[id as usize] = rank as u32; } for r in ranks.iter() {
for x in unsafe { &mut *r.get() }.iter_mut() { *x = rank_of[*x as usize]; } } unsafe { *gnames.get() = sorted }; } bar.wait(); let names: &[Name] = unsafe { &*gnames.get() }; let ne = names.len();
let mut start = vec![0u32; ne + 1]; let mut errs = vec![0u32; ne]; let mut cursor = vec![0u32; ne]; let mut latency: Vec<u32> = Vec::new(); loop { let h = claim.fetch_add(1, Ordering::Relaxed);
if h >= HOURS { break; } start.fill(0); errs.fill(0); for (i, psc) in parsers.iter().enumerate() { let ps = unsafe { &*psc.get() }; let rank = unsafe { &*ranks[i].get() };
ps.each(h, rank, |e, err, _| { unsafe { *start.get_unchecked_mut(e as usize + 1) += 1; *errs.get_unchecked_mut(e as usize) += (err != 0) as u32; } }); } for e in 0..ne { start[e + 1] += start[e]; }
latency.clear(); latency.resize(start[ne] as usize, 0); cursor.copy_from_slice(&start[..ne]); for (i, psc) in parsers.iter().enumerate() { let ps = unsafe { &*psc.get() };
let rank = unsafe { &*ranks[i].get() }; ps.each(h, rank, |e, _, l| unsafe { let c = cursor.get_unchecked_mut(e as usize); *latency.get_unchecked_mut(*c as usize) = l; *c += 1; }); }
finished[h].store(true, Ordering::Release); if !releasing.swap(true, Ordering::AcqRel) { for psc in parsers.iter() { unsafe { (*psc.get()).release_prefix(&finished) }; }
releasing.store(false, Ordering::Release); } let mut size = 0u64; let mut lines: Vec<Line> = Vec::with_capacity(2048); for e in 0..ne { let n = start[e + 1] - start[e]; if n == 0 { continue; }
let a = &mut latency[start[e] as usize..start[e + 1] as usize]; let (p50, p95, p99) = percentiles(a); let er = errs[e]; let ppm = (er as u64 * 1_000_000 / n as u64) as u32;
lines.push(Line { e: e as u32, n, p50, p95, p99, errors: er, ppm }); size += (LABEL + names[e].len as usize + 7) as u64 + width(n) + width(p50) + width(p95) + width(p99) + width(er) + width(ppm); }
unsafe { *hour_lines[h].get() = lines; *hour_off[h].get() = size; } } bar.wait(); unsafe { madvise(me.arena as *mut u8, me.next_chunk * CHUNK_WORDS * 4, MADV_DONTNEED); } if t == 0 {
let mut total = 0u64; for h in 0..HOURS { let sz = unsafe { *hour_off[h].get() }; unsafe { *hour_off[h].get() = total }; total += sz; } unsafe { *hour_off[HOURS].get() = total };
std::fs::create_dir_all(out_dir).ok(); let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(format!("{out_dir}/summary.txt")).expect("create summary.txt");
if total > 0 { unsafe { if ftruncate(f.as_raw_fd(), total as i64) != 0 { eprintln!("ftruncate failed: {:?}", std::io::Error::last_os_error()); std::process::exit(1); }
let m = mmap(std::ptr::null_mut(), total as usize, PROT_READ | PROT_WRITE, MAP_SHARED, f.as_raw_fd(), 0); if m as isize == -1 { eprintln!("output mmap failed: {:?}", std::io::Error::last_os_error());
std::process::exit(1); } text.store(m as usize, Ordering::Release); } } } bar.wait(); loop { let h = claim_text.fetch_add(1, Ordering::Relaxed); if h >= HOURS { break; }
let lines = unsafe { &*hour_lines[h].get() }; if lines.is_empty() { continue; } let label = hour_label(h);
let mut o = unsafe { (text.load(Ordering::Acquire) as *mut u8).add(*hour_off[h].get() as usize) }; for l in lines { unsafe { std::ptr::copy_nonoverlapping(label.as_ptr(), o, LABEL); o = o.add(LABEL);
let nm = names[l.e as usize]; std::ptr::copy_nonoverlapping(nm.ptr, o, nm.len as usize); o = o.add(nm.len as usize); for v in [l.n, l.p50, l.p95, l.p99, l.errors, l.ppm] { *o = b' '; o = o.add(1);
o = write_u32(o, v); } *o = b'\n'; o = o.add(1); } } unsafe { *hour_lines[h].get() = Vec::new() }; } }); } }); }
