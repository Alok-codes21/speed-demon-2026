// Speed Demon 2026, Track C (Join). Linux, C++20, no dependencies.
//
// mmap records.csv -> 12 threads parse 96 granules claimed from a counter
// -> merge per-thread aggregates -> one buffered write. Names intern to small
// ids in a lock-free hash table; each country keeps a bitset over name ids.
// Record walk and name-key layout re-implemented from ideas in Cache Me's
// MIT-licensed public entry (github.com/manusingh090/speed-demon-2026-entry):
// word-wise comma/digit decode, 64-byte quote stepping, 4-wide SIMD slot
// probe, per-record file prefetch 2KB ahead, distinct counted off the mark.

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <numeric>
#include <string>
#include <fcntl.h>
#include <immintrin.h>
#include <atomic>
#include <pthread.h>
#include <sys/prctl.h>
#include <sys/mman.h>
#include <sys/stat.h>

// table geometry: 2^17 name slots; 256 country slots of 2^17 bits
static const uint32_t HM = 131071, MAX_C = 256, BITS_W = 2048;
static const char *fend, *gst[97]; // records.csv end; granule boundaries

struct Stats {
  uint64_t count = 0, id_sum = 0, name_bytes = 0, note_bytes = 0, distinct = 0;
  int64_t amount_sum = 0, amount_min = INT64_MAX, amount_max = INT64_MIN;
};

static uint64_t (*CB)[BITS_W]; // per-country bitsets over name ids
static uint8_t *R2C;           // region id -> country slot
static uint32_t CK[MAX_C], NC = 0, UNMATCHED = 0; // packed codes; '_' unmatched
static uint64_t *NT; // name hash table; slot 0 reserved (id 0 = empty name)
static uint32_t gn; // granule counter

// Names intern to slot ids in a lock-free open-addressing table; the slot
// index is the id (all hashes have bit 63 set, so a plain CAS publishes the
// slot and no value is ever pending). k==0 below: our CAS installed h.
static inline uint32_t intern_name(uint64_t h, uint32_t idx) {
  for (idx &= ~3u;; idx = (idx + 4) & HM) {
    alignas(32) uint64_t snap[4]; // per-slot atomic loads keep this TSAN-clean
    for (int t = 0; t < 4; t++)
      snap[t] = __atomic_load_n(&NT[idx + t], __ATOMIC_ACQUIRE);
    __m256i v = _mm256_load_si256((const __m256i *)snap);
    uint32_t hit = _mm256_movemask_pd(_mm256_castsi256_pd(
        _mm256_cmpeq_epi64(v, _mm256_set1_epi64x(h))));
    if (hit)
      return idx + __builtin_ctz(hit);
    uint32_t free = _mm256_movemask_pd(_mm256_castsi256_pd(
        _mm256_cmpeq_epi64(v, _mm256_setzero_si256())));
    for (; free; free &= free - 1) {
      uint32_t j = idx + __builtin_ctz(free);
      if (__sync_bool_compare_and_swap(&NT[j], 0, h) ||
          __atomic_load_n(&NT[j], __ATOMIC_ACQUIRE) == h)
        return j;
    }
  }
}

// Name key: 64-bit hash of the raw field bytes ("" pairs included - raw is
// canonical). The first 16 bytes come in one masked SSE load; longer names
// fold in 8-byte words. With the spare page after EOF all loads are
// unconditional. The result is never 0 (0 marks a free slot).
static inline uint64_t hs(const char *s, uint32_t len) {
  static const signed char KEEP[32] = {-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1};
  __m128i head = _mm_loadu_si128((const __m128i *)s);
  head = _mm_and_si128(head, _mm_loadu_si128((const __m128i *)(KEEP + 16 - std::min(len, 16u))));
  uint64_t h = ((uint64_t)_mm_cvtsi128_si64(head) * 0x9E3779B97F4A7C15ULL ^ len) +
               (uint64_t)_mm_extract_epi64(head, 1) * 0xBF58476D1CE4E5B9ULL;
  for (uint32_t i = 16; i < len; i += 8) {
    uint64_t w;
    memcpy(&w, s + i, 8);
    if (len - i < 8)
      w &= (1ULL << 8 * (len - i)) - 1;
    h = (h ^ w) * 0x94D049BB133111EBULL;
  }
  h = (h ^ h >> 31) * 0xD6E8FEB86659FD93ULL;
  return (h ^ h >> 32) | (1ULL << 63);
}

// 32-byte mask of byte c at p
#define MM2(p, c) (_mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256((const __m256i *)(p)), _mm256_set1_epi8(c))))
// branch-free conversion of n (1..8) known digit bytes; reads 8 bytes at s
#define DIGS(s, n) ({uint64_t v_; memcpy(&v_, (s), 8); v_ <<= 64 - 8 * (n); v_ -= 0x3030303030303030ULL & (~0ULL << (64 - 8 * (n))); v_ = v_ * 2561 >> 8, v_ = (v_ & 0x00FF00FF00FF00FFULL) * 6553601 >> 16; (uint32_t)((v_ & 0x0000FFFF0000FFFFULL) * 42949672960001ULL >> 32);})

// The comma after a number of at most 7 digits starting at p: the lowest
// zero byte of the 8 bytes at p XOR ','.
static inline const char *comma_after(const char *p) {
  uint64_t x;
  memcpy(&x, p, 8);
  x ^= 0x2C2C2C2C2C2C2C2CULL;
  return p + __builtin_ctzll((x - 0x0101010101010101ULL) & ~x & 0x8080808080808080ULL) / 8;
}

// The value of the last n (1..8) digit bytes in v (first byte is lowest).
static inline uint64_t fold(uint64_t v, uint32_t n) {
  v &= 0x0F0F0F0F0F0F0F0FULL & (~0ULL << 8 * (8 - n));
  v = (v * 10 + (v >> 8)) & 0x00FF00FF00FF00FFULL;
  v = (v * 100 + (v >> 16)) & 0x0000FFFF0000FFFFULL;
  return (v * 10000 + (v >> 32)) & 0xFFFFFFFFULL;
}
// The 1..8 decimal digits in [s, e): one overlapping 8-byte load.
static inline uint64_t digits(const char *s, const char *e) {
  uint64_t w;
  memcpy(&w, e - 8, 8);
  return fold(w, (uint32_t)(e - s));
}
// Amount in cents from [s, e), dot third from the end: at most 7 digits, so
// the 8 bytes ending at e with the dot's byte dropped are one number.
static inline uint64_t money(const char *s, const char *e) {
  uint64_t w;
  memcpy(&w, e - 8, 8);
  return fold((w & 0xFFFFFFFFFFULL) << 8 | (w & 0xFFFF000000000000ULL),
              (uint32_t)(e - s - 1));
}

// field-width integer ending at term; consumes the terminator
static inline uint64_t num(const char *&p, char term) {
  uint64_t v = 0;
  while (*p != term)
    v = v * 10 + (*p++ - '0');
  return p++, v;
}

// ---------------------------------------------------------------- regions.csv

void load_regions(const std::string &path) {
  struct stat st;
  fstat(open(path.c_str(), O_RDONLY), &st);
  const char *data = (const char *)mmap(0, st.st_size, 1, 2, open(path.c_str(), O_RDONLY), 0),
               *end = data + st.st_size,
               *p = (const char *)memchr(data, '\n', st.st_size) + 1; // header
  int16_t slot_of[1024]; // country code -> slot, by first appearance
  memset(slot_of, -1, sizeof(slot_of)), memset(R2C, 255, 1000000);
  // lines are "rid,XXX\n" with rid < 8 digits: comma from one word's zero
  // bytes, digits decoded branch-free; ~4x faster than byte loops.
  for (uint64_t rid; p < end && p + 16 <= end + 8; p += 4) {
    uint64_t x;
    memcpy(&x, p, 8);
    x ^= 0x2C2C2C2C2C2C2C2CULL;
    uint32_t cpos = __builtin_ctzll((x - 0x0101010101010101ULL) & ~x & 0x8080808080808080ULL) / 8;
    rid = DIGS(p, cpos), p += cpos + 1;
    uint32_t w, key = (memcpy(&w, p, 4), __builtin_bswap32(w) >> 8),
              h = (key * 0x9E3779B1u) >> 22;
    while (slot_of[h] >= 0 && CK[slot_of[h]] != key)
      h = (h + 1) & 1023;
    slot_of[h] < 0 && (CK[slot_of[h] = (int16_t)NC++] = key, 0);
    rid < 1000000 && (R2C[rid] = (uint8_t)slot_of[h], 0);
  }
  CK[UNMATCHED = NC++] = 0x5F0000; // '_' sorts after every [A-Z] code
}

// -------------------------------------------------------------------- parsing

// 32-byte quote mask; the constant hoists out of the loop
#define MM(s) \
  _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256((const __m256i *)(s)), _mm256_set1_epi8('"')))

// A name or note at p, moved past its comma: raw bytes (between the quotes,
// if quoted) and the value length (one less per "" pair). Unquoted ends at
// the next comma, 64 bytes at a time; quoted steps through its quote mask,
// a "" pair costing one value byte, the first lone quote closing the field.
struct Text { const char *at; uint32_t raw, length; };
static inline Text text(const char *&p, const char *end) {
  const char *s = p;
  if (*s != '"') {
    p = (const char *)memchr(s, ',', end - s) + 1;
    return {s, (uint32_t)(p - 1 - s), (uint32_t)(p - 1 - s)};
  }
  uint32_t pairs = 0;
  const char *q = s + 1;
  for (uint64_t m = (uint64_t)(uint32_t)MM2(q, '"') | (uint64_t)(uint32_t)MM2(q + 32, '"') << 32;;) {
    uint32_t i = m ? __builtin_ctzll(m) : 64;
    if (i >= 63) { // none in view, or the byte after it is: refetch there
      q += i;
      m = (uint64_t)(uint32_t)MM2(q, '"') | (uint64_t)(uint32_t)MM2(q + 32, '"') << 32;
      continue;
    }
    if (!(m >> (i + 1) & 1)) {
      q += i;
      break;
    }
    ++pairs;
    m &= m - 1, m &= m - 1;
  }
  p = q + 2;
  return {s + 1, (uint32_t)(q - s - 1), (uint32_t)(q - s - 1 - pairs)};
}

struct Worker {
  Stats stats[MAX_C];
  int tid;
};
static char *pool;      // 2MB-aligned base of NT/R2C/CB
static size_t popbytes; // NT + CB bytes to pre-populate
static uint32_t nready;

// CR never appears inside a quoted field, so every "\r\n" ends a record and
// chunks cut exactly on record boundaries. Rows have no embedded CRLF.
__attribute__((aligned(64))) void *parse_chunk(void *arg) {
  Stats *stats = ((Worker *)arg)->stats;
  // Pre-populate this thread's share of the shared tables: twelve threads
  // faulting the same pages in races queue on the kernel's page-table lock,
  // and a read-then-written page faults twice (zero page, then private).
  {
    int T = ((Worker *)arg)->tid;
    size_t pages = popbytes / 4096, from = pages * T / 12, to = pages * (T + 1) / 12;
    if (to > from)
      madvise(pool + from * 4096, (to - from) * 4096, 23); // MADV_POPULATE_WRITE
    __atomic_fetch_add(&nready, 1, 0);
    while (__atomic_load_n(&nready, 0) < 12)
      ;
  }
  // Every record's three random reads (region table, name slot, bitset word)
  // go through a 16-record software pipeline: prefetched when parsed, the
  // country read + sums + intern retire 16 later, the mark 16 after that.
  struct Pend {
    uint64_t nh, id;
    int64_t cents;
    uint32_t slot, reg, name_len, note_len;
  } pend[16]; // 16-record deferral ring
  struct Mk { uint32_t c, bnid; } mk[16];
  uint64_t n = 0, last = ~0ULL;
  auto adv = [&]() __attribute__((always_inline)) {
    uint32_t k = n & 15;
    if (n >= 32 && n - 32 < last) {
      Mk &m2 = mk[k];
      uint64_t bit = 1ULL << (m2.bnid & 63);
      stats[m2.c].distinct +=
          !(__atomic_fetch_or(&CB[m2.c][m2.bnid >> 6], bit, 0) & bit);
    }
    if (n >= 16 && n - 16 < last) {
      Pend &o = pend[k];
      uint8_t c = (o.reg < 1000000) ? R2C[o.reg] : 255;
      Stats &st = stats[c = c == 255 ? UNMATCHED : c];
      st.count++, st.id_sum += o.id, st.amount_sum += o.cents;
      st.amount_min = std::min(st.amount_min, o.cents),
      st.amount_max = std::max(st.amount_max, o.cents);
      st.name_bytes += o.name_len, st.note_bytes += o.note_len;
      mk[k] = {c, o.name_len ? intern_name(o.nh, o.slot) : 0};
      __builtin_prefetch(&CB[mk[k].c][mk[k].bnid >> 6]);
    }
  };
  for (uint32_t g; (g = __atomic_fetch_add(&gn, 1, 0)) < 96;) {
    const char *p = gst[g], *end = gst[g + 1];
    while (p < end) {
      __builtin_prefetch(p + 2048, 0, 3);
      const char *s;
      uint32_t slot;
      p += 3 * ((uint8_t)*p == 0xEF); // BOM before a record
      const char *f1 = comma_after(p); // id: at most 8 digits
      uint64_t id = digits(p, f1);
      p = f1 + 1;
      Text nm = text(p, end), nt = text(p, end);
      uint64_t nh = nm.length ? hs(nm.at, nm.raw) : 1; // 1: empty name's fixed key
      __builtin_prefetch(&NT[slot = nh & HM]);
      // region (<=7 digits) and amount (<=9 bytes) end within 32 bytes at p
      uint32_t lf = MM2(p, '\n');
      const char *f4 = p + __builtin_ctz(MM2(p, ','));
      uint32_t reg = (uint32_t)digits(p, f4);
      const char *amt = f4 + 1, *eol = lf ? p + __builtin_ctz(lf) : end,
                 *ae = eol - (eol[-1] == '\r');
      int64_t neg = *amt == '-', cents = money(amt + neg, ae);
      cents = neg ? -cents : cents;
      __builtin_prefetch(&R2C[reg < 1000000 ? reg : 0]);
      adv();
      pend[n++ & 15] = {nh, id, cents, slot, reg, nm.length, nt.length};
      p = eol + 1;
    }
  }
  for (last = n; n < last + 32; ++n)
    adv();
  return nullptr;
}

// -------------------------------------------------------------------- output

static void pr_money(FILE *f, int64_t c, char sep) {
  c < 0 && (fputc('-', f), c = -c, 0);
  fprintf(f, "%zu.%02u%c", (size_t)(c / 100), (uint32_t)(c % 100), sep);
}

int main(int argc, char **argv) {
  prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0); // 4KB pages: populate the used set exactly
  std::string data_dir = argc > 1 ? argv[1] : "/data",
              out_dir = argc > 2 ? argv[2] : "/out";
  // 8MB anon (RW, private); align to 2MB so 6MB of tables sit on 3 huge pages
  char *big = (char *)mmap(0, 8 << 20, 3, 0x22, -1, 0);
  size_t al = (size_t)(big + (2 << 20) - 1) & -(2 << 20);
  // NT 1MB, R2C 1MB, CB the rest; slot 0 reserved (real hashes set bit 63)
  NT = (uint64_t *)al, R2C = (uint8_t *)(al + (1 << 20)),
  CB = (uint64_t(*)[BITS_W])(al + (2 << 20)), NT[0] = 1, pool = (char *)al;
  load_regions(data_dir + "/regions.csv");
  popbytes = (2 << 20) + (size_t)NC * BITS_W * 8; // NT + R2C + used CB
  int rfd = open((data_dir + "/records.csv").c_str(), O_RDONLY);
  struct stat st;
  fstat(rfd, &st);
  char *res = (char *)mmap(0, st.st_size + (2 << 20) + 4096, 3, 0x22, -1, 0);
  size_t ra = (size_t)(res + (2 << 20) - 1) & -(2 << 20);
  const char *data = (const char *)mmap((void *)ra, st.st_size, 3, 0x12, rfd, 0);
  madvise((void *)data, st.st_size, 2),
      madvise((void *)data, st.st_size, 14), // SEQUENTIAL | HUGEPAGE
      fend = data + st.st_size;

  // Split into 96 granules at exact record boundaries; workers pull granules
  // from an atomic counter, so a slow granule cannot leave a tail idle.
  gst[0] = (const char *)memchr(data, '\n', st.st_size) + 1, gst[96] = fend;
  size_t total = fend - gst[0];
  for (int i = 1; i < 96; i++) {
    const char *ap = gst[0] + (i * total) / 96,
               *m = (const char *)memmem(ap, fend - ap, "\r\n", 2);
    gst[i] = m ? m + 2 : fend;
  }

  static Worker workers[12];
  pthread_t ths[12];
  for (int i = 0; i < 12; i++)
    workers[i].tid = i;
  for (int i = 0; i < 12; i++)
    pthread_create(&ths[i], nullptr, parse_chunk, &workers[i]);
  for (int i = 0; i < 12; i++)
    pthread_join(ths[i], nullptr);

  // Merge the per-thread aggregates (empty threads merge as zeros).
  Stats final_s[MAX_C];
  for (uint32_t i = 0; i < 12 * NC; i++) {
    auto &w = workers[i / NC].stats[i % NC], &f = final_s[i % NC];
    f.count += w.count, f.id_sum += w.id_sum, f.amount_sum += w.amount_sum;
    f.amount_min = std::min(f.amount_min, w.amount_min),
    f.amount_max = std::max(f.amount_max, w.amount_max);
    f.name_bytes += w.name_bytes, f.note_bytes += w.note_bytes, f.distinct += w.distinct;
  }

  // Countries sort by their packed 3-byte key; '_' (UNMATCHED) is last.
  uint32_t order[MAX_C];
  std::iota(order, order + NC, 0u);
  std::sort(order, order + NC, [](auto a, auto b) { return CK[a] < CK[b]; });

  mkdir(out_dir.c_str(), 0777);
  FILE *out = fopen((out_dir + "/aggregates.txt").c_str(), "w");
  out || (exit(1), 0);
  for (uint32_t oi = 0; oi < NC; oi++) {
    uint32_t c = order[oi], d = (uint32_t)final_s[c].distinct;
    const Stats &g = final_s[c];
    if (g.count == 0)
      continue;
    char code[4] = {(char)(CK[c] >> 16), (char)(CK[c] >> 8), (char)CK[c], 0};
    fprintf(out, "%s %lu %lu ", c == UNMATCHED ? "_UNMATCHED" : code, g.count,
            g.id_sum),
        pr_money(out, g.amount_sum, ' '), pr_money(out, g.amount_min, ' '),
        pr_money(out, g.amount_max, ' ');
    fprintf(out, "%u %lu %lu\n", d, g.name_bytes, g.note_bytes);
  }
  return fclose(out), 0;
}


