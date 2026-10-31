// Speed Demon 2026 - Track A: Ingest (Team Cosmic).
//
// std-only Rust, AVX2; mmap/madvise/fadvise reached through extern "C".
//
// Streaming pipeline (no phase barriers):
//   1. The log is mapped read-only (huge pages when available) over one
//      guard zero page and claimed in 256 KiB line-aligned blocks; each
//      block is parsed as four interleaved streams so one line's
//      dictionary probe overlaps the next line's decode.
//   2. A line decodes with raw-ASCII timestamp arithmetic, one 64-byte SIMD
//      probe for the two spaces, a shared lock-free-read endpoint
//      dictionary, and a digit-count latency decode that never searches
//      for the line feed. Records pack into 4 bytes (latency | 5xx flag |
//      endpoint id) and land in per-hour run buffers recycled from a pool.
//   3. Blocks register their runs in file order; a watermark tracks the
//      highest hour seen in the registered prefix. Once the watermark is
//      LAG hours past an hour, that hour is finalized on the spot: a
//      counting sort by endpoint (in catalogue byte order, which is the
//      output order), three nested selections for p50/p95/p99, and text
//      formatting. Finalization overlaps the scan, so only a few hours of
//      records are ever live.
//   4. Finished hour texts are written in order by whoever finds the
//      writer free - outside the lock, so no finalize blocks behind the
//      kernel - and written pages are dropped with fadvise two hours
//      behind the frontier. At the end each worker drops its share of the
//      input mapping so process exit has no page table to tear down.
//   5. The log is only nearly ordered: a line arriving after its hour was
//      finalized sets a flag, and the whole file is reprocessed with an
//      infinite lag (finalize nothing until every block is parsed), which
//      is exact for any input order.

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;

const HOURS: usize = 72;
const SLOTS: usize = 8192;        // shared dictionary slots, power of two
const MAX_NAMES: usize = 2048;    // endpoint ids must fit 12 bits with the flag
const LABEL: usize = 21;          // "2026-10-30T18:00:00Z "
const BLOCK: usize = 256 << 10;   // parse granularity
const AHEAD: usize = 48;          // at most this many blocks past the watermark
const STREAMS: usize = 4;         // interleaved parse streams per block
const RUN_RECS: usize = BLOCK / 40 + 16; // one record per shortest line fits
const REC: usize = 6;            // record: u32 latency, u16 id | 5xx << 11
const LAG: usize = 2;             // hours behind the watermark it is safe to finalize
const NEVER: usize = 1000;        // fallback lag: nothing finalizes mid-scan

const PROT_READ: i32 = 1;
const MAP_PRIVATE: i32 = 0x02;
const MAP_FIXED: i32 = 0x10;
const MAP_ANON: i32 = 0x20;
const FADV_DONTNEED: i32 = 4;

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn posix_fadvise(fd: i32, off: i64, len: i64, advice: i32) -> i32;
    fn madvise(addr: *mut u8, len: usize, advice: i32) -> i32;
}
const MADV_DONTNEED: i32 = 4;
const MADV_HUGEPAGE: i32 = 14;

// ------------------------------------------------------------- byte helpers

#[derive(Clone, Copy)]
struct Name {
    ptr: *const u8,
    len: u32,
}

#[inline(always)]
fn name_eq(a: Name, b: Name) -> bool {
    if a.len != b.len {
        return false;
    }
    let n = a.len as usize;
    if n <= 32 {
        unsafe {
            use std::arch::x86_64::*;
            let x = _mm256_loadu_si256(a.ptr as *const __m256i);
            let y = _mm256_loadu_si256(b.ptr as *const __m256i);
            let eq = _mm256_movemask_epi8(_mm256_cmpeq_epi8(x, y)) as u32;
            let mask = if n == 32 { u32::MAX } else { (1u32 << n) - 1 };
            return !eq & mask == 0;
        }
    }
    unsafe { std::slice::from_raw_parts(a.ptr, n) == std::slice::from_raw_parts(b.ptr, n) }
}

#[inline(always)]
unsafe fn find_byte(mut s: *const u8, c: u8) -> *const u8 {
    use std::arch::x86_64::*;
    let v = _mm256_set1_epi8(c as i8);
    loop {
        let m = _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256(s as *const __m256i), v));
        if m != 0 {
            return s.add(m.trailing_zeros() as usize);
        }
        s = s.add(32);
    }
}

// From one byte past the timestamp, locate the endpoint's start and end.
// Both spaces usually sit in one 64-byte window; a near-maximum endpoint
// takes the slow scan. The line feed is never searched: take_latency counts
// the digits itself and the next line starts right after them.
#[inline(always)]
unsafe fn scan_line(m: *const u8) -> (*const u8, *const u8) {
    {
        use std::arch::x86_64::*;
        let a = _mm256_loadu_si256(m as *const __m256i);
        let b = _mm256_loadu_si256(m.add(32) as *const __m256i);
        let sp = (_mm256_movemask_epi8(_mm256_cmpeq_epi8(a, _mm256_set1_epi8(0x20))) as u32 as u64)
            | ((_mm256_movemask_epi8(_mm256_cmpeq_epi8(b, _mm256_set1_epi8(0x20))) as u32 as u64) << 32);
        let s1 = sp.trailing_zeros();
        let rest = sp & (sp - 1);
        if rest != 0 {
            return (m.add(s1 as usize + 1), m.add(rest.trailing_zeros() as usize));
        }
        let name = m.add(s1 as usize + 1);
        let name_end = find_byte(name, b' ');
        (name, name_end)
    }
}

// Up to 8 decimal digits ending just before `end`, branch-free.
#[inline(always)]
unsafe fn dec8(end: *const u8, len: usize) -> u32 {
    let w = (end.sub(8) as *const u64).read_unaligned().swap_bytes();
    let w = if len == 8 { w } else { w & ((1u64 << (8 * len)) - 1) };
    let v = w & 0x0F0F_0F0F_0F0F_0F0F;
    let pairs = (v & 0x00FF_00FF_00FF_00FF).wrapping_add(((v >> 8) & 0x00FF_00FF_00FF_00FF).wrapping_mul(10));
    let quads = (pairs & 0x0000_FFFF_0000_FFFF).wrapping_add(((pairs >> 16) & 0x0000_FFFF_0000_FFFF).wrapping_mul(100));
    (quads & 0xFFFF_FFFF) as u32 + (quads >> 32) as u32 * 10_000
}

// Parse the decimal latency starting at `s`, return (value, next line).
// The digit count comes from the same 8-byte load as the digits: xor with
// '0' then (v + 0x76) | v sets the high bit exactly on non-digit bytes, so
// ctz/8 counts digits without ever looking for the line feed.
#[inline(always)]
unsafe fn take_latency(s: *const u8) -> (u32, *const u8) {
    let v = (s as *const u64).read_unaligned() ^ 0x3030_3030_3030_3030;
    let other = (v.wrapping_add(0x7676_7676_7676_7676) | v) & 0x8080_8080_8080_8080;
    if other != 0 {
        let digits = (other.trailing_zeros() >> 3) as usize;
        (dec8(s.add(digits), digits), s.add(digits + 1))
    } else {
        let mut lat = dec8(s.add(8), 8);
        let mut s = s.add(8);
        while *s != b'\n' {
            lat = lat * 10 + (*s - b'0') as u32;
            s = s.add(1);
        }
        (lat, s.add(1))
    }
}

// ---------------------------------------------------- shared name dictionary

// Every worker interns against one table, so a line's global endpoint id is
// known at parse time and no merge or remap phase exists. Reads are
// lock-free; the rare insert takes a spinlock and publishes with a release
// store. A slot packs (id + 1) | len << 16; names live in a fixed image
// table, so ids and pointers never move.
struct Dict {
    slots: Vec<AtomicU64>,
    img: Vec<std::cell::UnsafeCell<[u8; 128]>>,
    lens: Vec<AtomicU64>,
    next: AtomicUsize,
    mu: Mutex<()>,
    sorted: std::cell::UnsafeCell<Vec<u32>>, // ids in byte order; under mu
}
unsafe impl Sync for Dict {}

impl Dict {
    fn new() -> Dict {
        Dict {
            slots: (0..SLOTS).map(|_| AtomicU64::new(0)).collect(),
            img: (0..MAX_NAMES).map(|_| std::cell::UnsafeCell::new([0u8; 128])).collect(),
            lens: (0..MAX_NAMES).map(|_| AtomicU64::new(0)).collect(),
            next: AtomicUsize::new(0),
            mu: Mutex::new(()),
            sorted: std::cell::UnsafeCell::new(Vec::new()),
        }
    }

    // Hash of the first and last eight bytes and the length.
    #[inline(always)]
    fn hash(s: Name) -> usize {
        let n = s.len as usize;
        unsafe {
            let mut a = (s.ptr as *const u64).read_unaligned();
            a = if n >= 8 { a } else { a & ((1u64 << (8 * n)) - 1) };
            let b = if n >= 8 { (s.ptr.add(n - 8) as *const u64).read_unaligned() } else { 0 };
            // One multiply: the first word, the last word rotated, and the
            // length already decorrelate the catalogue's shared prefixes.
            let h = (a ^ b.rotate_left(31) ^ n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            (h >> 29) as usize
        }
    }

    #[inline(always)]
    fn img_name(&self, id: u32) -> Name {
        Name {
            ptr: self.img[id as usize].get() as *const u8,
            len: self.lens[id as usize].load(Ordering::Relaxed) as u32,
        }
    }

    #[inline(always)]
    fn intern(&self, s: Name) -> u32 {
        let mut i = Self::hash(s) & (SLOTS - 1);
        loop {
            let v = self.slots[i].load(Ordering::Acquire);
            if v == 0 {
                return self.insert(s);
            }
            let id = (v & 0xffff) as u32 - 1;
            if name_eq(self.img_name(id), s) {
                return id;
            }
            i = (i + 1) & (SLOTS - 1);
        }
    }

    #[cold]
    fn insert(&self, s: Name) -> u32 {
        let _g = self.mu.lock().unwrap();
        let mut i = Self::hash(s) & (SLOTS - 1);
        loop {
            let v = self.slots[i].load(Ordering::Acquire);
            if v == 0 {
                let id = self.next.fetch_add(1, Ordering::Relaxed) as u32;
                assert!((id as usize) < MAX_NAMES, "more endpoints than the spec's catalogue");
                unsafe {
                    std::ptr::copy_nonoverlapping(s.ptr, self.img[id as usize].get() as *mut u8, s.len as usize);
                    self.lens[id as usize].store(s.len as u64, Ordering::Relaxed);
                    // Insert the id into the byte-ordered list.
                    let sorted = &mut *self.sorted.get();
                    let sn = std::slice::from_raw_parts(s.ptr, s.len as usize);
                    let at = sorted.partition_point(|&o| {
                        let on = &*self.img[o as usize].get();
                        let ol = self.lens[o as usize].load(Ordering::Relaxed) as usize;
                        on[..ol].cmp(sn) == std::cmp::Ordering::Less
                    });
                    sorted.insert(at, id);
                }
                self.slots[i].store((id as u64 + 1) | ((s.len as u64) << 16), Ordering::Release);
                return id;
            }
            let id = (v & 0xffff) as u32 - 1;
            if name_eq(self.img_name(id), s) {
                return id;
            }
            i = (i + 1) & (SLOTS - 1);
        }
    }
}

// ------------------------------------------------------------------ output

#[inline(always)]
unsafe fn write_u32(o: *mut u8, v: u32) -> *mut u8 {
    let mut buf = [0u8; 10];
    let mut i = 10;
    let mut v = v;
    while v >= 100 {
        let d = (v % 100) as usize * 2;
        v /= 100;
        i -= 2;
        buf[i] = DIGIT_PAIRS[d];
        buf[i + 1] = DIGIT_PAIRS[d + 1];
    }
    if v < 10 {
        i -= 1;
        buf[i] = b'0' + v as u8;
    } else {
        i -= 2;
        buf[i] = DIGIT_PAIRS[v as usize * 2];
        buf[i + 1] = DIGIT_PAIRS[v as usize * 2 + 1];
    }
    let n = 10 - i;
    std::ptr::copy_nonoverlapping(buf.as_ptr().add(i), o, n);
    o.add(n)
}

static DIGIT_PAIRS: [u8; 200] = *b"00010203040506070809101112131415161718192021222324252627282930313233343536373839404142434445464748495051525354555657585960616263646566676869707172737475767778798081828384858687888990919293949596979899";

fn hour_label(h: usize) -> [u8; LABEL] {
    let t = h + 18;
    let d = t / 24;
    let hh = (t % 24) as u8;
    let (mo, dy) = [(10u8, 30u8), (10, 31), (11, 1), (11, 2)][d];
    let mut b = *b"2026-10-30T18:00:00Z ";
    b[6] = b'0' + mo % 10;
    b[8] = b'0' + dy / 10;
    b[9] = b'0' + dy % 10;
    b[11] = b'0' + hh / 10;
    b[12] = b'0' + hh % 10;
    b
}

// Nearest-rank p50/p95/p99. Small groups sort outright; larger ones take
// three nested linear selections, highest rank first.
#[inline(always)]
fn percentiles(a: &mut [u32]) -> (u32, u32, u32) {
    let n = a.len();
    let r50 = (50 * n + 99) / 100 - 1;
    let r95 = (95 * n + 99) / 100 - 1;
    let r99 = (99 * n + 99) / 100 - 1;
    if n <= 64 {
        a.sort_unstable();
        return (a[r50], a[r95], a[r99]);
    }
    let p99 = *a.select_nth_unstable(r99).1;
    let lo = &mut a[..r99];
    let p95 = if r95 == r99 { p99 } else { *lo.select_nth_unstable(r95).1 };
    let lo2 = &mut a[..r95];
    let p50 = if r50 >= lo2.len() { p95 } else { *lo2.select_nth_unstable(r50).1 };
    (p50, p95, p99)
}

// ---------------------------------------------------------- streaming engine

type Run = (*mut u8, u32); // one hour's records from one block

struct Reg {
    top: Vec<i32>,          // per block: its highest hour, -1 until registered
    watermark: usize,       // blocks below this are all registered
    prefix_max: i32,        // highest top over the registered prefix
    registered: usize,
    runs: [Vec<Run>; HOURS],
    next_hour: usize,       // next hour to finalize
    bad: bool,              // a line arrived after its hour finalized
    pool: Vec<*mut u8>,     // recycled run buffers
}

struct Out {
    parts: [Option<Vec<u8>>; HOURS],
    written: usize,
    writing: bool,
    off: i64,
    ends: [i64; HOURS + 1],      // output offset after each hour
    text_pool: Vec<Vec<u8>>,     // recycled per-hour text buffers
    lat_pool: Vec<Vec<u32>>,     // recycled per-hour scatter buffers
}

struct Engine {
    log: *const u8,
    size: usize,
    file: File,
    nblocks: usize,
    lag: usize,
    threads: usize,
    dict: Dict,
    mu: Mutex<Reg>,
    cv: Condvar,
    next_block: AtomicUsize,
    out: Mutex<Out>,
}
unsafe impl Sync for Engine {}

impl Engine {
    fn claim(&self) -> Claim {
        let mut r = self.mu.lock().unwrap();
        loop {
            if r.bad {
                return Claim::Done;
            }
            if r.next_hour < HOURS
                && (r.registered == self.nblocks || (r.next_hour + self.lag) as i32 <= r.prefix_max)
            {
                let h = r.next_hour;
                r.next_hour += 1;
                return Claim::Finalize(h);
            }
            let b = self.next_block.load(Ordering::Relaxed);
            if b < self.nblocks && b < r.watermark + AHEAD {
                if self.next_block.compare_exchange(b, b + 1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                    return Claim::Parse(b);
                }
                continue;
            }
            if r.next_hour >= HOURS && self.next_block.load(Ordering::Relaxed) >= self.nblocks {
                return Claim::Done;
            }
            r = self.cv.wait(r).unwrap();
        }
    }

    // Parse one block into per-hour run buffers and register them.
    fn parse(&self, b: usize) {
        let start = unsafe { self.log.add(self.align(b)) };
        let end = unsafe { self.log.add(self.align(b + 1)) };
        let mut scan = BlockScan {
            engine: self,
            wp: [std::ptr::null_mut(); HOURS],
            bp: [std::ptr::null_mut(); HOURS],
        };
        // Four line-aligned streams parsed in round-robin: the dictionary
        // probe of one line overlaps the next line's decode, hiding the
        // load latency a single stream would serialize on.
        let mut pos = [start; STREAMS];
        let mut stop = [end; STREAMS];
        for i in 1..STREAMS {
            let mut c = unsafe { start.add((end as usize - start as usize) * i / STREAMS) };
            while c < end && unsafe { *c.sub(1) } != b'\n' {
                c = unsafe { c.add(1) };
            }
            pos[i] = c;
            stop[i - 1] = c;
        }
        while pos.iter().zip(stop.iter()).all(|(p, s)| p < s) {
            for i in 0..STREAMS {
                pos[i] = unsafe { scan.line(pos[i]) };
            }
        }
        for i in 0..STREAMS {
            while pos[i] < stop[i] {
                pos[i] = unsafe { scan.line(pos[i]) };
            }
        }
        let BlockScan { wp, bp, .. } = scan;
        // Register runs in file order and advance the watermark.
        let mut r = self.mu.lock().unwrap();
        let mut last = -1i32;
        for h in 0..HOURS {
            if !bp[h].is_null() {
                last = last.max(h as i32);
                if h < r.next_hour {
                    r.bad = true;
                    r.pool.push(bp[h]);
                } else {
                    let n = (unsafe { wp[h].offset_from(bp[h]) } as u32) / REC as u32;
                    r.runs[h].push((bp[h], n));
                }
            }
        }
        r.top[b] = last;
        r.registered += 1;
        while r.watermark < self.nblocks && r.top[r.watermark] >= 0 {
            r.prefix_max = r.prefix_max.max(r.top[r.watermark]);
            r.watermark += 1;
        }
        drop(r);
        self.cv.notify_all();
    }

    fn take_buf(&self) -> *mut u8 {
        // +8: the last record's single 8-byte store spills past its 6 bytes.
        let mut r = self.mu.lock().unwrap();
        match r.pool.pop() {
            Some(p) => p,
            None => unsafe {
                std::alloc::alloc(std::alloc::Layout::array::<u8>(RUN_RECS * REC + 8).unwrap())
            },
        }
    }

    fn align(&self, b: usize) -> usize {
        let mut c = (b * BLOCK).min(self.size);
        if b > 0 {
            while c < self.size && unsafe { *self.log.add(c - 1) } != b'\n' {
                c += 1;
            }
        }
        c
    }

    // Counting-sort one hour's records by endpoint (in byte order, the output
    // order), select percentiles per group, format the hour's text.
    fn finalize(&self, h: usize) {
        let (runs, order) = {
            let mut r = self.mu.lock().unwrap();
            let runs = std::mem::take(&mut r.runs[h]);
            let _g = self.dict.mu.lock().unwrap();
            let order = unsafe { (*self.dict.sorted.get()).clone() };
            (runs, order)
        };
        let mut count = vec![0u32; 2 * MAX_NAMES];
        for &(buf, n) in &runs {
            for i in 0..n as usize {
                let tag = unsafe { (buf.add(i * REC + 4) as *const u16).read_unaligned() };
                count[(tag & 0xfff) as usize] += 1;
            }
        }
        // Segment offsets in endpoint byte order; errors share the id segment.
        let mut pos = vec![0u32; MAX_NAMES];
        let mut total = 0u32;
        for &id in &order {
            pos[id as usize] = total;
            total += count[id as usize] + count[id as usize + MAX_NAMES];
        }
        let mut lat: Vec<u32> = {
            let mut o = self.out.lock().unwrap();
            o.lat_pool.pop().unwrap_or_default()
        };
        lat.clear();
        lat.reserve(total as usize);
        #[allow(clippy::uninit_vec)]
        unsafe {
            lat.set_len(total as usize);
        }
        for &(buf, n) in &runs {
            for i in 0..n as usize {
                let tag = unsafe { (buf.add(i * REC + 4) as *const u16).read_unaligned() };
                let c = &mut pos[(tag & 0x7ff) as usize];
                lat[*c as usize] = unsafe { (buf.add(i * REC) as *const u32).read_unaligned() };
                *c += 1;
            }
        }

        let label = hour_label(h);
        let mut text: Vec<u8> = {
            let mut o = self.out.lock().unwrap();
            o.text_pool.pop().unwrap_or_default()
        };
        text.clear();
        text.reserve(1 << 16);
        let mut start = 0u32;
        for &id in &order {
            let idu = id as usize;
            let errors = count[idu + MAX_NAMES];
            let n = count[idu] + errors;
            if n == 0 {
                continue;
            }
            let a = &mut lat[start as usize..(start + n) as usize];
            start += n;
            let (p50, p95, p99) = percentiles(a);
            let ppm = (errors as u64 * 1_000_000 / n as u64) as u32;
            unsafe {
                text.reserve(LABEL + 128 + 6 * 12);
                let start_len = text.len();
                let mut o = text.as_mut_ptr().add(start_len);
                std::ptr::copy_nonoverlapping(label.as_ptr(), o, LABEL);
                o = o.add(LABEL);
                let nm = self.dict.img_name(id);
                std::ptr::copy_nonoverlapping(nm.ptr, o, nm.len as usize);
                o = o.add(nm.len as usize);
                for v in [n, p50, p95, p99, errors, ppm] {
                    *o = b' ';
                    o = o.add(1);
                    o = write_u32(o, v);
                }
                *o = b'\n';
                o = o.add(1);
                text.set_len(o.offset_from(text.as_ptr()) as usize);
            }
        }

        lat.clear();
        {
            let mut r = self.mu.lock().unwrap();
            for &(buf, _) in &runs {
                r.pool.push(buf);
            }
        }
        self.out.lock().unwrap().lat_pool.push(lat);

        // Deposit the part; whoever finds the writer idle drains in order.
        // The write runs WITHOUT the lock - while `writing` is set the writer
        // owns the file position and the other threads only deposit into
        // parts[], so no finalize ever blocks behind the kernel.
        let mut out = self.out.lock().unwrap();
        let fd = self.file.as_raw_fd();
        out.parts[h] = Some(text);
        if out.writing {
            return;
        }
        out.writing = true;
        loop {
            let w = out.written;
            if w >= HOURS {
                break;
            }
            let Some(part) = out.parts[w].take() else { break };
            let off = out.off as u64;
            drop(out);
            use std::os::unix::fs::FileExt;
            self.file.write_all_at(&part, off).expect("write summary.txt");
            let mut o = self.out.lock().unwrap();
            o.off += part.len() as i64;
            o.ends[w + 1] = o.off;
            o.written += 1;
            if w >= 2 {
                // Drop cached output pages two hours behind the frontier.
                unsafe {
                    posix_fadvise(fd, 0, o.ends[w - 1], FADV_DONTNEED);
                }
            }
            o.text_pool.push(part);
            out = o;
        }
        out.writing = false;
    }

    fn run(&self) -> bool {
        thread::scope(|s| {
            for i in 0..self.threads {
                s.spawn(move || {
                    loop {
                        match self.claim() {
                            Claim::Parse(b) => self.parse(b),
                            Claim::Finalize(h) => self.finalize(h),
                            Claim::Done => break,
                        }
                    }
                    // Work is over: drop this thread's share of the input
                    // mapping now, in parallel, so process exit has no
                    // populated page table left to tear down.
                    let slice = (self.size / self.threads + 4095) / 4096 * 4096;
                    let from = (i * slice).min(self.size);
                    unsafe {
                        madvise(self.log.add(from) as *mut u8, slice.min(self.size - from), MADV_DONTNEED);
                    }
                });
            }
        });
        let r = self.mu.lock().unwrap();
        !r.bad
    }
}

// Per-block scan state: the open run buffer per hour.
struct BlockScan<'e> {
    engine: &'e Engine,
    wp: [*mut u8; HOURS],
    bp: [*mut u8; HOURS],
}

impl BlockScan<'_> {
    // Parse the line at `p`, return the next line's start.
    #[inline(always)]
    unsafe fn line(&mut self, p: *const u8) -> *const u8 {
        // YYYY-MM-DDTHH: raw-ASCII hour index, branch-free. The day units
        // digit (p[9]) plus the month parity (p[6]&1: 0 for October, 1 for
        // November) times 24, plus the two hour digits; the constant folds
        // every ASCII offset and the Oct-30-18:00 origin.
        let h = (*p.add(9) as usize + (*p.add(6) & 1) as usize) * 24
            + (*p.add(11) as usize) * 10
            + *p.add(12) as usize
            - 1698;

        let (name, name_end) = scan_line(p.add(28));
        let id = self.engine.dict.intern(Name {
            ptr: name,
            len: (name_end as usize - name as usize) as u32,
        });
        let key = id | if *name_end.add(1) == b'5' { 0x800 } else { 0 };
        let (latency, next) = take_latency(name_end.add(5));

        let mut w = self.wp[h];
        if w.is_null() {
            w = self.engine.take_buf();
            self.wp[h] = w;
            self.bp[h] = w;
        }
        // One 8-byte store covers the 6-byte record; the next record's store
        // rewrites the two spilled bytes (the buffer has 8 bytes of slack).
        std::ptr::write_unaligned(w as *mut u64, latency as u64 | (key as u64) << 32);
        self.wp[h] = w.add(REC);
        next
    }
}

enum Claim {
    Parse(usize),
    Finalize(usize),
    Done,
}

// --------------------------------------------------------------------- main

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = args.get(1).map(String::as_str).unwrap_or("/data");
    let out_dir = args.get(2).map(String::as_str).unwrap_or("/out");
    let nt = std::env::var("SD_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| thread::available_parallelism().map(|x| x.get()).unwrap_or(1));

    let input = File::open(format!("{data_dir}/requests.log")).expect("open requests.log");
    let size = input.metadata().expect("stat").len() as usize;

    // Map the log read-only over one guard zero page so the wide scans and
    // fixed-size loads near the end stay in bounds.
    let log = unsafe {
        let region = mmap(std::ptr::null_mut(), size + 4096, PROT_READ, MAP_PRIVATE | MAP_ANON, -1, 0);
        if region as isize == -1 {
            eprintln!("input anon mmap failed");
            std::process::exit(1);
        }
        if size > 0 && mmap(region, size, PROT_READ, MAP_PRIVATE | MAP_FIXED, input.as_raw_fd(), 0) as isize == -1 {
            eprintln!("input file mmap failed");
            std::process::exit(1);
        }
        region
    };
    // Back the log with huge pages when the kernel has them: the scan faults
    // ~120 2 MiB pages instead of ~58k 4 KiB ones. Advisory; the kernel
    // falls back to small pages under fragmentation.
    unsafe {
        madvise(log, size, MADV_HUGEPAGE);
    }

    for lag in [LAG, NEVER] {
        std::fs::create_dir_all(out_dir).ok();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(format!("{out_dir}/summary.txt"))
            .expect("create summary.txt");
        let nblocks = size.div_ceil(BLOCK);
        let engine = Engine {
            log,
            size,
            nblocks,
            lag,
            threads: nt,
            dict: Dict::new(),
            mu: Mutex::new(Reg {
                top: vec![-1; nblocks],
                watermark: 0,
                prefix_max: -1,
                registered: 0,
                runs: std::array::from_fn(|_| Vec::new()),
                next_hour: 0,
                bad: false,
                pool: Vec::new(),
            }),
            file,
            cv: Condvar::new(),
            next_block: AtomicUsize::new(0),
            out: Mutex::new(Out {
                parts: std::array::from_fn(|_| None),
                written: 0,
                writing: false,
                off: 0,
                ends: [0; HOURS + 1],
                text_pool: Vec::new(),
                lat_pool: Vec::new(),
            }),
        };
        let ok = engine.run();

        if ok {
            return;
        }
        // A line arrived after its hour was finalized: reprocess the whole
        // file, finalizing nothing until every block is parsed.
    }
}
