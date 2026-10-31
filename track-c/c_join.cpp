// Speed Demon 2026, Track C (Join). Linux, C++20, no dependencies.
//
// mmap records.csv -> 12 threads parse disjoint granules at record boundaries
// -> merge per-thread aggregates -> one buffered write. Two dedup phases:
// phase 1 interns names in a shared read-mostly table (CAS only for never-
// seen names) and appends 4-byte (slot, country) keys to thread-local chunked
// streams - no locked RMWs in the record loop; after a barrier, phase 2 deals
// countries to threads by decreasing size and counts distinct ids in a
// private 16KB bitset per country (L1-resident).

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <numeric>
#include <string>
#include <fcntl.h>
#include <immintrin.h>
#include <pthread.h>
#include <sys/mman.h>
#include <sys/stat.h>

// table geometry: 2^17 name slots; 256 country slots of 2^17 bits
static const uint32_t HM = 131071, MAX_C = 256, BITS_W = 2048;
static const char *fend, *gst[193]; // records.csv end; granule boundaries

struct Stats {
  uint64_t count = 0, id_sum = 0, name_bytes = 0, note_bytes = 0;
  int64_t amount_sum = 0, amount_min = INT64_MAX, amount_max = INT64_MIN;
};

static uint8_t *R2C;           // region id -> country slot
static uint32_t CK[MAX_C], NC = 0, UNMATCHED = 0; // packed codes; '_' unmatched
static uint64_t *NT; // name hash table; slot 0 reserved (id 0 = empty name)
static uint32_t gn; // granule counter

// Dedup streams: each (thread, country) pair owns a chain of ~1KB chunks of
// 32-bit (slot << 8 | country) keys; chunks come from a shared pool with an
// atomic bump pointer (one atomic per 254 records, none per record).
struct Chunk {
  Chunk *next;
  uint32_t keys[254];
};
struct Stream {
  Chunk *head = nullptr, *cur = nullptr;
  uint32_t n = 254; // forces chunk allocation on the first append
};
static Chunk *pool;
static uint32_t pool_n, pool_cap;
static uint32_t dcnt[MAX_C];
static pthread_barrier_t bar;

// Names intern to slot ids in a lock-free open-addressing table; the slot
// index is the id (all hashes have bit 63 set, so a plain CAS publishes the
// slot and no value is ever pending). k==0 below: our CAS installed h.
static inline uint32_t intern_name(uint64_t h, uint32_t idx) {
  for (uint64_t k = __atomic_load_n(&NT[idx], __ATOMIC_ACQUIRE);;
       idx = (idx + 1) & HM, k = __atomic_load_n(&NT[idx], __ATOMIC_ACQUIRE))
    if ((k = k ? k : __sync_val_compare_and_swap(&NT[idx], 0, h)) == h || !k)
      return idx;
}

// 8 bytes with the high (8 - n) bytes zeroed, never reading past end.
static inline uint64_t load8(const char *s, uint32_t n, const char *end) {
  uint64_t v = 0;
  if (__builtin_expect(s + 8 <= end, 1))
    memcpy(&v, s, 8);
  else {
    char tmp[8] = {};
    memcpy(tmp, s, n);
    memcpy(&v, tmp, 8);
  }
  return n < 8 ? v & ((1ULL << (8 * n)) - 1) : v;
}

// Fast path: a name of at most 25 bytes packs into four 64-bit words; 94% of
// names are <= 16 bytes, so only w0/w1 are usually loaded. The 128-bit
// multiply fold gives a strong avalanche in one multiply (mum-style mixer).
static inline uint64_t hs(const char *s, uint32_t len, const char *end) {
  uint64_t w0 = load8(s, std::min(len, 8u), end),
           w1 = load8(s + 8, std::min(len > 8 ? len - 8 : 0, 8u), end),
           h = w0 ^ w1 * 0x9e3779b97f4a7c15ULL ^ len * 0xbf58476d1ce4e5b9ULL;
  if (__builtin_expect(len > 16, 0))
    h ^= load8(s + 16, std::min(len - 16, 8u), end) * 0x517cc1b727220a95ULL ^
         (len > 24 ? (uint8_t)s[24] * 0x85ebca6bULL : 0);
  __uint128_t r = (__uint128_t)h * 0x9e3779b97f4a7c15ULL;
  return ((uint64_t)r ^ (uint64_t)(r >> 64)) | (1ULL << 63);
}

// Everything else (escaped or over 25 bytes): full-length FNV-1a over the
// unescaped value, so distinct names are never merged by truncation and no
// unescape buffer is needed. "" decodes to one quote. Rare on real data.
static uint64_t hl(const char *s, const char *e) {
  uint64_t h = 1469598103934665603ULL;
  while (s < e)
    h = (h ^ (uint8_t)*s++) * 1099511628211ULL;
  return h | (1ULL << 63);
}


// 32-byte mask of byte c at s
static inline uint32_t mmch(const char *s, char c) {
  return _mm256_movemask_epi8(_mm256_cmpeq_epi8(
      _mm256_loadu_si256((const __m256i *)s), _mm256_set1_epi8(c)));
}

// branch-free conversion of n (1..8) known digit bytes; reads 8 bytes at s
static inline uint64_t digs(const char *s, uint32_t n) {
  uint64_t v;
  memcpy(&v, s, 8);
  v <<= 64 - 8 * n;
  v -= 0x3030303030303030ULL & (~0ULL << (64 - 8 * n));
  v = v * 2561 >> 8, v = (v & 0x00FF00FF00FF00FFULL) * 6553601 >> 16;
  return (uint32_t)((v & 0x0000FFFF0000FFFFULL) * 42949672960001ULL >> 32);
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
  for (uint64_t rid; p < end && (rid = num(p, ','), true); p += 4) {
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
  Stream streams[MAX_C];
};
static Worker workers[12];

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
  Worker *wk = (Worker *)arg;
  Stats *stats = wk->stats;
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
                                                 : hs(s, name_len, fend))
                        : 0;
      __builtin_prefetch(&NT[slot = nh & HM]);
      QF(note_len); // note: only its unescaped byte length is needed
      uint32_t reg;
      int64_t cents;
      bool fast = false;
      if (__builtin_expect(p + 32 <= end, 1)) {
        uint32_t mc = mmch(p, ','), mn = mmch(p, '\n');
        if (__builtin_expect(mc && mn, 1)) {
          uint32_t c3 = __builtin_ctz(mc);          // region width
          uint32_t nl = __builtin_ctz(mn);          // record end
          uint32_t ae = nl - (p[nl - 1] == '\r');   // one past fraction
          uint32_t a0 = c3 + 1 + (p[c3 + 1] == '-'); // amount int start
          if (__builtin_expect(c3 - 1 < 8 && nl >= a0 + 4 &&
                                   p[ae - 3] == '.' && ae - 3 - a0 - 1 < 8, 1)) {
            reg = digs(p, c3);
            int64_t neg = p[c3 + 1] == '-';
            cents = digs(p + a0, ae - 3 - a0) * 100 + (p[ae - 2] - '0') * 10 +
                    (p[ae - 1] - '0');
            cents = neg ? -cents : cents;
            p += nl + 1;
            fast = true;
          }
        }
      }
      if (__builtin_expect(!fast, 0)) { // long fields or file tail: scalar
        reg = num(p, ',');
        int64_t neg = *p == '-';
        cents = num(p += neg, '.') * 100 + (p[0] - '0') * 10 + (p[1] - '0');
        p += 2, cents = neg ? -cents : cents; // two fraction digits consumed
        p += p < end ? 1 + (*p == '\r') : 0;  // "\r\n" or "\n", bare at eof
      }
      uint8_t c = (reg < 1000000) ? R2C[reg] : 255;

      // join and aggregate
      Stats &st = stats[c = c == 255 ? UNMATCHED : c];
      st.count++, st.id_sum += id, st.amount_sum += cents;
      st.amount_min = cents < st.amount_min ? cents : st.amount_min;
      st.amount_max = cents > st.amount_max ? cents : st.amount_max;
      st.name_bytes += name_len, st.note_bytes += note_len;
      uint32_t nid = name_len ? intern_name(nh, slot) : 0;
      // append the (slot, country) key to this thread's stream for c
      Stream &sm = wk->streams[c];
      if (__builtin_expect(sm.n == 254, 0)) {
        Chunk *nc = &pool[__atomic_fetch_add(&pool_n, 1, 0)];
        if (__builtin_expect(sm.cur == nullptr, 0))
          sm.head = nc;
        else
          sm.cur->next = nc;
        sm.cur = nc, sm.n = 0;
      }
      sm.cur->keys[sm.n++] = (nid << 8) | c;
    }
  }

  // Phase 2: after all parsing, countries are dealt to threads by decreasing
  // size (each thread recomputes the same deal); a thread counts distinct
  // name ids per country in a private L1-resident bitset.
  pthread_barrier_wait(&bar);
  uint64_t ctot[MAX_C] = {};
  for (uint32_t c = 0; c < NC; c++)
    for (auto &w : workers)
      ctot[c] += w.stats[c].count;
  uint32_t order[MAX_C];
  std::iota(order, order + NC, 0u);
  std::sort(order, order + NC,
            [&](auto a, auto b) { return ctot[a] > ctot[b]; });
  uint64_t bits[BITS_W];
  uint32_t tid = (uint32_t)(wk - workers);
  for (uint32_t oi = tid; oi < NC; oi += 12) {
    uint32_t c = order[oi];
    if (!ctot[c])
      continue;
    memset(bits, 0, sizeof(bits));
    for (auto &w : workers)
      for (Chunk *ch = w.streams[c].head; ch; ch = ch->next) {
        uint32_t lim = ch->next ? 254 : w.streams[c].n;
        for (uint32_t i = 0; i < lim; i++)
          bits[ch->keys[i] >> 14] |= 1ULL << ((ch->keys[i] >> 8) & 63);
      }
    uint32_t d = 0;
    for (uint32_t i = 0; i < BITS_W; i++)
      d += __builtin_popcountll(bits[i]);
    dcnt[c] = d;
  }
  return nullptr;
}

// -------------------------------------------------------------------- output

static void pr_money(FILE *f, int64_t c, char sep) {
  c < 0 && (fputc('-', f), c = -c, 0);
  fprintf(f, "%zu.%02u%c", (size_t)(c / 100), (uint32_t)(c % 100), sep);
}

int main(int argc, char **argv) {
  std::string data_dir = argc > 1 ? argv[1] : "/data",
              out_dir = argc > 2 ? argv[2] : "/out";
  // Anon RW region, 2MB-aligned for huge pages: R2C 1MB, NT 1MB, then the
  // chunk pool for the dedup streams (pages are charged only when touched).
  char *big = (char *)mmap(0, 64 << 20, 3, 0x22, -1, 0);
  size_t al = (size_t)(big + (2 << 20) - 1) & -(2 << 20);
  madvise((void *)al, 60 << 20, 14); // MADV_HUGEPAGE
  R2C = (uint8_t *)al, NT = (uint64_t *)(al + (1 << 20)), NT[0] = 1;
  pool = (Chunk *)(al + (2 << 20));
  pool_cap = (58u << 20) / sizeof(Chunk), pool_n = 0;
  load_regions(data_dir + "/regions.csv");
  struct stat st;
  fstat(open((data_dir + "/records.csv").c_str(), O_RDONLY), &st);
  const char *data = (const char *)mmap(
      0, st.st_size, 1, 2, open((data_dir + "/records.csv").c_str(), O_RDONLY), 0);
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

  pthread_barrier_init(&bar, nullptr, 13);
  pthread_t ths[12];
  for (int i = 0; i < 12; i++)
    pthread_create(&ths[i], nullptr, parse_chunk, &workers[i]);
  pthread_barrier_wait(&bar); // parsing done; phase 2 runs inside the workers
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
    uint32_t c = order[oi], d = dcnt[c];
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
