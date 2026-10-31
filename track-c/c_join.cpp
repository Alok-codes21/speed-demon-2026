// Speed Demon 2026, Track C (Join). Linux, C++20, no dependencies.
//
// mmap records.csv -> 12 threads parse disjoint granules at record boundaries
// -> merge per-thread aggregates -> one buffered write. Names intern to small
// ids in a lock-free hash table; each country keeps a bitset over name ids.

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
static const char *fend, *gst[193]; // records.csv end; granule boundaries

struct Stats {
  uint64_t count = 0, id_sum = 0, name_bytes = 0, note_bytes = 0;
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
  for (uint64_t k = __atomic_load_n(&NT[idx], __ATOMIC_ACQUIRE);;
       idx = (idx + 1) & HM, k = __atomic_load_n(&NT[idx], __ATOMIC_ACQUIRE))
    if ((k = k ? k : __sync_val_compare_and_swap(&NT[idx], 0, h)) == h || !k)
      return idx;
}

// Names hash over raw field bytes ("" pairs included - canonical). With the
// spare page after EOF, all 8-byte loads are unconditional: no end guard.
// <= 25-byte names pack into three masked words; w2 masks to zero for short
// names, so the mix is branch-free and the hash value is unchanged.
#define LENM(k) ((k) > 7 ? ~0ULL : (1ULL << (8 * (k))) - 1)
static inline uint64_t hs(const char *s, uint32_t len) {
  uint64_t w0, w1 = 0, w2 = 0; // short names skip the upper loads entirely
  memcpy(&w0, s, 8), w0 &= LENM(std::min(len, 8u));
  if (len > 8)
    memcpy(&w1, s + 8, 8), w1 &= LENM(std::min(len - 8, 8u));
  if (len > 16)
    memcpy(&w2, s + 16, 8), w2 &= LENM(len - 16);
  uint64_t h = w0 ^ w1 * 0x9e3779b97f4a7c15ULL ^ w2 * 0x517cc1b727220a95ULL ^
               len * 0xbf58476d1ce4e5b9ULL;
  __uint128_t r = (__uint128_t)h * 0x9e3779b97f4a7c15ULL;
  return ((uint64_t)r ^ (uint64_t)(r >> 64)) | (1ULL << 63);
}

// Over 25 bytes or escaped: word-wise multiply-mix over [s, e) with an
// overlapping tail read (raw length always exceeds 24 here, so e - 8 >= s).
static uint64_t hl(const char *s, const char *e) {
  uint64_t h = ((uint64_t)(e - s)) * 0x9e3779b97f4a7c15ULL;
  for (; s + 8 <= e; s += 8) {
    uint64_t w;
    memcpy(&w, s, 8);
    h = (h ^ w) * 0xbf58476d1ce4e5b9ULL;
  }
  uint64_t w;
  memcpy(&w, e - 8, 8);
  h = (h ^ w * 0x517cc1b727220a95ULL) * 0xbf58476d1ce4e5b9ULL;
  return (h ^ h >> 32) | (1ULL << 63);
}

// 32-byte mask of byte c at p
#define MM2(p, c) (_mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256((const __m256i *)(p)), _mm256_set1_epi8(c))))
// branch-free conversion of n (1..8) known digit bytes; reads 8 bytes at s
#define DIGS(s, n) ({uint64_t v_; memcpy(&v_, (s), 8); v_ <<= 64 - 8 * (n); v_ -= 0x3030303030303030ULL & (~0ULL << (64 - 8 * (n))); v_ = v_ * 2561 >> 8, v_ = (v_ & 0x00FF00FF00FF00FFULL) * 6553601 >> 16; (uint32_t)((v_ & 0x0000FFFF0000FFFFULL) * 42949672960001ULL >> 32);})

// field-width integer ending at term; consumes the terminator
static inline uint64_t num(const char *&p, char term) {
  uint64_t v = 0;
  while (*p != term)
    v = v * 10 + (*p++ - '0');
  return p++, v;
}

// ---------------------------------------------------------------- regions.csv
// Loaded by the worker pool, in parallel: each thread SWAR-parses a slice of
// lines (comma via the zero-byte trick, digits branch-free), claims country
// slots from a lock-free code->slot table, and stores region->slot. Slot
// numbers are arbitrary: output sorts by the packed code, never by slot.
static int32_t slot_of[1024]; // country code -> slot, CAS-published (-2 = busy)
static const char *rbs[13];   // region-file slice boundaries for 12 threads
static inline void reg_slice(const char *p, const char *end) {
  uint32_t n;
  for (; p < end; p += n + 5) { // rid(1..6 digits) ',' CCC '\n'
    uint64_t x;
    memcpy(&x, p, 8);
    uint64_t c = x ^ 0x2C2C2C2C2C2C2C2CULL,
             m = (c - 0x0101010101010101ULL) & ~c & 0x8080808080808080ULL;
    uint32_t rid, w;
    n = (uint32_t)__builtin_ctzll(m) >> 3, rid = (uint32_t)DIGS(p, n);
    memcpy(&w, p + n + 1, 4);
    uint32_t key = __builtin_bswap32(w) >> 8, h = (key * 0x9E3779B1u) >> 22;
    for (;; h = (h + 1) & 1023) {
      int32_t s = __atomic_load_n(&slot_of[h], __ATOMIC_ACQUIRE);
      if (s == -2)
        continue;
      if (s < 0) {
        if (!__sync_bool_compare_and_swap(&slot_of[h], -1, -2))
          continue;
        uint32_t sl = __atomic_fetch_add(&NC, 1, 0);
        CK[sl] = key;
        __atomic_store_n(&slot_of[h], (int32_t)sl, __ATOMIC_RELEASE), s = (int32_t)sl;
      }
      if (CK[s] == key) {
        rid < 1000000 && (R2C[rid] = (uint8_t)s, 0);
        break;
      }
    }
  }
}

// -------------------------------------------------------------------- parsing

// 32-byte quote mask; the constant hoists out of the loop
#define MM(s) \
  _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_loadu_si256((const __m256i *)(s)), _mm256_set1_epi8('"')))
// Next quote; AVX2 scan with a memchr tail. End-bounded.
static inline const char *find_quote(const char *s, const char *end) {
  for (; s + 32 <= end; s += 32)
    if (int m = MM(s))
      return s + __builtin_ctz(m);
  return (const char *)memchr(s, '"', end - s);
}

// End of the quoted field whose value starts at s: one past the comma after
// the closing quote, or nullptr on malformed input. *esc counts "" pairs.
static inline const char *quoted_end(const char *s, const char *end,
                                     uint32_t *esc) {
  const char *q = (*esc = 0, find_quote(s, end));
  while (__builtin_expect(q != nullptr && q[1] == '"', 0)) // "" = one quote
    ++*esc, q = find_quote(q + 2, end);
  return q ? q + 2 : nullptr;
}

struct Worker {
  Stats stats[MAX_C];
  int tid;
};
static char *pool;      // 2MB-aligned base of NT/R2C/CB
static uint32_t nready;

// CR never appears inside a quoted field, so every "\r\n" ends a record and
// chunks cut exactly on record boundaries. Rows have no embedded newlines.
// QF: parse one quoted-or-empty field at p into LEN, advancing p; sets s
// (field start), e (one past the comma), esc ("" pair count). Breaks on
// malformed input.
#define QF(LEN)                                      \
  if (*p == '"') {                                   \
    s = ++p, e = quoted_end(s, end, &esc);           \
    if (__builtin_expect(e == nullptr, 0))           \
      break;                                         \
    LEN = (uint32_t)(e - 2 - s) - esc, p = e;        \
  } else                                             \
    esc = 0, p++;
__attribute__((aligned(64))) void *parse_chunk(void *arg) {
  Stats *stats = ((Worker *)arg)->stats;
  // Phases: clear R2C + populate NT/R2C -> load regions.csv slices ->
  // populate the CB now that NC is known -> parse. Twelve threads faulting
  // the same pages in races queue on the kernel's page-table lock, so every
  // shared table is populated by all threads up front, each its own share.
  int T = ((Worker *)arg)->tid;
  for (int phase = 0; phase < 3; phase++) {
    if (phase == 1)
      reg_slice(rbs[T], rbs[T + 1]);
    else {
      size_t bytes = phase == 0 ? 2 << 20 : (size_t)NC * BITS_W * 8,
             pages = bytes / 4096, from = pages * T / 12, to = pages * (T + 1) / 12;
      char *base = phase == 0 ? pool : (char *)CB;
      if (to > from)
        madvise(base + from * 4096, (to - from) * 4096, 23); // MADV_POPULATE_WRITE
      if (phase == 0)
        memset(R2C + (1000000 / 12) * T, 255,
               T == 11 ? 1000000 - (1000000 / 12) * 11 : 1000000 / 12);
    }
    __atomic_fetch_add(&nready, 1, __ATOMIC_ACQ_REL);
    while (__atomic_load_n(&nready, __ATOMIC_ACQUIRE) < 12 * (phase + 1))
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
  auto adv = [&]<bool D>() __attribute__((always_inline)) {
    uint32_t k = n & 15; // D=false in the main loop: the drain guards vanish
    if (n >= 32 && (D ? n - 32 < last : true)) {
      Mk &m2 = mk[k];
      __atomic_fetch_or(&CB[m2.c][m2.bnid >> 6], 1ULL << (m2.bnid & 63), 0);
    }
    if (n >= 16 && (D ? n - 16 < last : true)) {
      Pend &o = pend[k];
      uint8_t c = (o.reg < 1000000) ? R2C[o.reg] : 255;
      Stats &st = stats[c = c == 255 ? UNMATCHED : c];
      st.count++, st.id_sum += o.id, st.amount_sum += o.cents,
      st.name_bytes += o.name_len, st.note_bytes += o.note_len,
      st.amount_min = std::min(st.amount_min, o.cents),
      st.amount_max = std::max(st.amount_max, o.cents);
      mk[k] = {c, o.name_len ? intern_name(o.nh, o.slot) : 0};
      __builtin_prefetch(&CB[mk[k].c][mk[k].bnid >> 6]);
    }
  };
  for (uint32_t g; (g = __atomic_fetch_add(&gn, 1, 0)) < 192;) {
    const char *p = gst[g], *end = gst[g + 1];
    while (p < end) {
      // skip a UTF-8 BOM before a record, then the id field
      const char *s, *e;
      uint32_t esc, name_len = 0, note_len = 0, slot;
      uint64_t id = (p += 3 * ((uint8_t)*p == 0xEF), num(p, ','));
      // name: empty unquoted, or quoted (non-empty names are always quoted)
      QF(name_len);
      // empty names never intern; skip the hash (s is only set by QF)
      uint64_t nh = name_len
                        ? (esc | (name_len > 25) ? hl(s, e - 2)
                                                 : hs(s, name_len))
                        : 0;
      __builtin_prefetch(&NT[slot = nh & HM]);
      QF(note_len); // note: only its unescaped byte length is needed
      // region + amount from one 32B delimiter window; scalar on long/tail
      uint32_t reg, mc, mn, c3, nl, ae, a0;
      int64_t cents, neg;
      if (p + 32 <= end && (mc = MM2(p, ',')) && (mn = MM2(p, '\n')) &&
          (c3 = __builtin_ctz(mc)) - 1 < 8 &&
          (nl = __builtin_ctz(mn)) >= (a0 = c3 + 1 + (p[c3 + 1] == '-')) + 4 &&
          p[(ae = nl - (p[nl - 1] == '\r')) - 3] == '.' && ae - 3 - a0 - 1 < 8)
        reg = DIGS(p, c3), neg = p[c3 + 1] == '-',
        cents = DIGS(p + a0, ae - 3 - a0) * 100 + (p[ae - 2] - '0') * 10 + (p[ae - 1] - '0'),
        cents = neg ? -cents : cents, p += nl + 1;
      else
        reg = num(p, ','), neg = *p == '-',
        cents = num(p += neg, '.') * 100 + (p[0] - '0') * 10 + (p[1] - '0'),
        p += 2, cents = neg ? -cents : cents,
        p += p < end ? 1 + (*p == '\r') : 0;
      __builtin_prefetch(&R2C[reg < 1000000 ? reg : 0]);
      adv.operator()<false>(); // retire ring slot n & 15 before reuse
      pend[n++ & 15] = {nh, id, cents, slot, reg, name_len, note_len};
    }
  }
  for (last = n; n < last + 32; ++n)
    adv.operator()<true>();
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
  memset(slot_of, -1, sizeof(slot_of));
  NC = 1, UNMATCHED = 0, CK[0] = 0x5F0000; // '_' sorts after every [A-Z] code
  int gfd = open((data_dir + "/regions.csv").c_str(), O_RDONLY);
  struct stat grst;
  fstat(gfd, &grst);
  char *gres = (char *)mmap(0, grst.st_size + (2 << 20) + 4096, 3, 0x22, -1, 0);
  size_t ga = (size_t)(gres + (2 << 20) - 1) & -(2 << 20);
  const char *gdata = (const char *)mmap((void *)ga, grst.st_size, 3, 0x12, gfd, 0);
  rbs[0] = (const char *)memchr(gdata, '\n', grst.st_size) + 1,
  rbs[12] = gdata + grst.st_size;
  for (int i = 1; i < 12; i++)
    rbs[i] = (const char *)memchr(rbs[0] + (i * (rbs[12] - rbs[0])) / 12, '\n',
                                  rbs[12] - rbs[0]) + 1;
  int rfd = open((data_dir + "/records.csv").c_str(), O_RDONLY);
  struct stat st;
  fstat(rfd, &st);
  char *res = (char *)mmap(0, st.st_size + (2 << 20) + 4096, 3, 0x22, -1, 0);
  size_t ra = (size_t)(res + (2 << 20) - 1) & -(2 << 20);
  const char *data = (const char *)mmap((void *)ra, st.st_size, 3, 0x12, rfd, 0);
  madvise((void *)data, st.st_size, 2),
      madvise((void *)data, st.st_size, 14), // SEQUENTIAL | HUGEPAGE
      fend = data + st.st_size;
  // Split into 192 granules at exact record boundaries; workers pull granules
  // from an atomic counter, so a slow granule cannot leave a tail idle.
  gst[0] = (const char *)memchr(data, '\n', st.st_size) + 1, gst[192] = fend;
  size_t total = fend - gst[0];
  for (int i = 1; i < 192; i++) {
    const char *ap = gst[0] + (i * total) / 192,
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
    f.name_bytes += w.name_bytes, f.note_bytes += w.note_bytes;
  }
  // Countries sort by their packed 3-byte key; '_' (UNMATCHED) is last.
  uint32_t order[MAX_C];
  std::iota(order, order + NC, 0u);
  std::sort(order, order + NC, [](auto a, auto b) { return CK[a] < CK[b]; });
  mkdir(out_dir.c_str(), 0777);
  FILE *out = fopen((out_dir + "/aggregates.txt").c_str(), "w");
  out || (exit(1), 0);
  for (uint32_t oi = 0; oi < NC; oi++) {
    uint32_t c = order[oi], d = 0;
    const Stats &g = final_s[c];
    if (g.count == 0)
      continue;
    for (uint32_t i = 0; i < BITS_W; i++)
      d += __builtin_popcountll(CB[c][i]);
    char code[4] = {(char)(CK[c] >> 16), (char)(CK[c] >> 8), (char)CK[c], 0};
    fprintf(out, "%s %lu %lu ", c == UNMATCHED ? "_UNMATCHED" : code, g.count,
            g.id_sum),
        pr_money(out, g.amount_sum, ' '), pr_money(out, g.amount_min, ' '),
        pr_money(out, g.amount_max, ' ');
    fprintf(out, "%u %lu %lu\n", d, g.name_bytes, g.note_bytes);
  }
  return fclose(out), 0;
}
