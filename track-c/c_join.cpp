// Speed Demon 2026, Track C (Join). Linux, C++20, no dependencies.
//
// mmap records.csv -> 12 threads parse disjoint granules at record boundaries
// -> merge per-thread aggregates -> one buffered write. Two phases: parse
// appends each record's (name, country) pair-key to a thread-local chunked
// stream per country (no shared-table traffic in the hot loop); after a
// barrier, countries are dealt to threads which count distinct keys in
// private hash sets.

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
#include <cstdlib>

static const uint32_t MAX_C = 256;
static const char *fend, *gst[193]; // records.csv end; granule boundaries

struct Stats {
  uint64_t count = 0, id_sum = 0, name_bytes = 0, note_bytes = 0;
  int64_t amount_sum = 0, amount_min = INT64_MAX, amount_max = INT64_MIN;
};

static uint8_t *R2C;           // region id -> country slot
static uint32_t CK[MAX_C], NC = 0, UNMATCHED = 0; // packed codes; '_' unmatched
static uint32_t gn; // granule counter

// Dedup streams: each (thread, country) pair owns a chain of 1KB chunks of
// 64-bit pair-keys; chunks come from a shared pool with an atomic bump
// pointer (one atomic per 127 records, none per record).
struct Chunk {
  Chunk *next;
  uint64_t keys[127];
};
struct Stream {
  Chunk *head = nullptr, *cur = nullptr;
  uint32_t n = 127; // forces chunk allocation on the first append
};
static Chunk *pool;
static uint32_t pool_n, pool_cap;
static uint32_t dcnt[MAX_C];
static pthread_barrier_t bar;

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
  uint64_t *pset = nullptr; // phase-2 scratch set
  uint32_t pcap = 0;
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
      uint32_t esc, name_len = 0, note_len = 0;
      uint64_t id = (p += 3 * ((uint8_t)*p == 0xEF), num(p, ','));
      // name: empty unquoted, or quoted (non-empty names are always quoted)
      QF(name_len);
      // empty names share one key per country; skip the hash for them
      uint64_t nh = name_len
                        ? (esc | (name_len > 25) ? hl(s, e - 2)
                                                 : hs(s, name_len, fend))
                        : 0;
      QF(note_len); // note: only its unescaped byte length is needed
      uint32_t reg = num(p, ',');
      uint8_t c = (reg < 1000000) ? R2C[reg] : 255;
      int64_t neg = *p == '-';
      int64_t cents = num(p += neg, '.') * 100 + (p[0] - '0') * 10 + (p[1] - '0');
      p += 2, cents = neg ? -cents : cents; // two fraction digits consumed
      p += p < end ? 1 + (*p == '\r') : 0;  // "\r\n" or "\n", bare at eof

      // join and aggregate
      Stats &st = stats[c = c == 255 ? UNMATCHED : c];
      st.count++, st.id_sum += id, st.amount_sum += cents;
      st.amount_min = cents < st.amount_min ? cents : st.amount_min;
      st.amount_max = cents > st.amount_max ? cents : st.amount_max;
      st.name_bytes += name_len, st.note_bytes += note_len;
      // append the pair-key to this thread's stream for the country
      Stream &sm = wk->streams[c];
      if (__builtin_expect(sm.n == 127, 0)) {
        Chunk *nc =
            &pool[__atomic_fetch_add(&pool_n, 1, 0)];
        if (__builtin_expect(sm.cur == nullptr, 0))
          sm.head = nc;
        else
          sm.cur->next = nc;
        sm.cur = nc, sm.n = 0;
      }
      sm.cur->keys[sm.n++] = nh ^ (0x9e3779b97f4a7c15ULL * (c + 1));
    }
  }

  // Phase 2: after all parsing, countries are dealt to threads by decreasing
  // size (each thread recomputes the same deal); a thread counts distinct
  // keys per country in a private open-addressing set.
  pthread_barrier_wait(&bar);
  uint64_t ctot[MAX_C] = {}, tload[MAX_C];
  for (uint32_t c = 0; c < NC; c++)
    for (auto &w : workers)
      ctot[c] += w.stats[c].count;
  uint32_t order[MAX_C];
  std::iota(order, order + NC, 0u);
  std::sort(order, order + NC,
            [&](auto a, auto b) { return ctot[a] > ctot[b]; });
  uint32_t tid = (uint32_t)(wk - workers);
  for (uint32_t oi = tid; oi < NC; oi += 12) {
    uint32_t c = order[oi];
    if (!ctot[c])
      continue;
    uint32_t need = 16;
    while (need < ctot[c] * 3 / 2)
      need *= 2;
    if (need > wk->pcap) {
      free(wk->pset);
      wk->pset = (uint64_t *)calloc(need, 8);
      wk->pcap = need;
    } else
      memset(wk->pset, 0, (size_t)wk->pcap * 8);
    uint64_t *t = wk->pset, d = 0;
    uint32_t mask = wk->pcap - 1;
    for (auto &w : workers)
      for (Chunk *ch = w.streams[c].head; ch; ch = ch->next) {
        uint32_t lim = ch->next ? 127 : w.streams[c].n;
        for (uint32_t i = 0; i < lim; i++) {
          uint64_t k = ch->keys[i] | (1ULL << 63);
          for (uint32_t j = k & mask;; j = (j + 1) & mask) {
            uint64_t v = t[j];
            if (!v) {
              t[j] = k, d++;
              break;
            }
            if (v == k)
              break;
          }
        }
      }
    dcnt[c] = (uint32_t)d;
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
  // 40MB anon (RW, private); align to 2MB for huge pages. R2C 1MB, then the
  // chunk pool for the dedup streams.
  char *big = (char *)mmap(0, 48 << 20, 3, 0x22, -1, 0);
  size_t al = (size_t)(big + (2 << 20) - 1) & -(2 << 20);
  madvise((void *)al, 40 << 20, 14); // MADV_HUGEPAGE
  R2C = (uint8_t *)al;
  pool = (Chunk *)(al + (1 << 20));
  pool_cap = (39u << 20) / sizeof(Chunk), pool_n = 0;
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
