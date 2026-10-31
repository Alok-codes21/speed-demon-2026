// Speed Demon 2026, Track C (Join). Linux, C++20, no dependencies.
//
// mmap records.csv -> 12 threads parse disjoint chunks at record boundaries
// -> merge per-thread aggregates -> one buffered write. Names are interned
// to small ids in a lock-free hash table; each country keeps a bitset over
// name ids for the distinct count.

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <fcntl.h>
#include <immintrin.h>
#include <pthread.h>
#include <string>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

static const uint32_t HASH_SIZE = 131072, HM = HASH_SIZE - 1;
// country slots, 64-bit words per bitset
static const uint32_t MAX_C = 256, BITS_W = HASH_SIZE / 64;
static const char *fend; // end of the records.csv mapping

struct Stats {
  uint64_t count = 0, id_sum = 0, name_bytes = 0, note_bytes = 0;
  int64_t amount_sum = 0, amount_min = INT64_MAX, amount_max = INT64_MIN;
};

static uint64_t (*CB)[BITS_W];
static uint8_t *R2C;
// packed 3-letter code per slot; '_' for _UNMATCHED
static uint32_t CK[MAX_C];
static uint32_t NC = 0, UNMATCHED = 0;
// slot 0 stays reserved: id 0 is the empty name
static uint64_t *NT;

// Names intern to slot ids in a lock-free open-addressing table; the slot
// index is the id (all hashes have bit 63 set, so a plain CAS publishes the
// slot and no value is ever pending). A per-thread cache measured useless:
// names have no temporal locality (2% hit rate).
static inline uint32_t intern_name(uint64_t h, uint32_t idx) {
  for (;; idx = (idx + 1) & HM)
    // k==0 below: our CAS installed h into an empty slot
    if (uint64_t k = __atomic_load_n(&NT[idx], __ATOMIC_ACQUIRE);
        (k = k ? k : __sync_val_compare_and_swap(&NT[idx], 0, h)) == h || !k)
      return idx;
}

// 8 bytes with the high (8 - n) bytes zeroed, never reading past end.
static inline uint64_t load8(const char *s, uint32_t n, const char *end) {
  uint64_t v = 0;
  if (__builtin_expect(s + 8 <= end, 1))
    memcpy(&v, s, 8);
  else {
    char tmp[8] = {};
    memcpy(tmp, s, n), memcpy(&v, tmp, 8);
  }
  return n < 8 ? v & ((1ULL << (8 * n)) - 1) : v;
}

// 128-bit multiply fold: strong avalanche in one multiply (mum-style mixer).
// Fast path: a name of at most 25 bytes packs into four 64-bit words; 94% of
// names are <= 16 bytes, so only w0/w1 are usually loaded.
static inline uint64_t hs(const char *s, uint32_t len, const char *end) {
  uint64_t w0 = load8(s, len < 8 ? len : 8, end);
  uint64_t w1 = len > 8 ? load8(s + 8, len - 8 < 8 ? len - 8 : 8, end) : 0;
  uint64_t h = w0 ^ (w1 * 0x9e3779b97f4a7c15ULL) ^
               ((uint64_t)len * 0xbf58476d1ce4e5b9ULL);
  if (__builtin_expect(len > 16, 0))
    h ^= load8(s + 16, std::min(len - 16, 8u), end) * 0x517cc1b727220a95ULL ^
         (len > 24 ? (uint64_t)(uint8_t)s[24] * 0x85ebca6bULL : 0);
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

// ---------------------------------------------------------------- regions.csv

void load_regions(const std::string &path) {
  struct stat st;
  int fd = open(path.c_str(), O_RDONLY);
  fstat(fd, &st);
  const char *data =
      (const char *)mmap(0, st.st_size, PROT_READ, MAP_PRIVATE, fd, 0),
               *end = data + st.st_size,
               *p = (const char *)memchr(data, '\n', st.st_size) + 1;
  int16_t slot_of[1024]; // country code -> slot, by first appearance
  memset(slot_of, -1, sizeof(slot_of)), memset(R2C, 255, 1000000);
  while (p < end) {
    uint64_t rid = 0, n = 0;
    while (p[n] != ',')
      rid = rid * 10 + (p[n++] - '0');
    uint32_t w, key = (memcpy(&w, p + n + 1, 4), __builtin_bswap32(w) >> 8);
    uint32_t h = (key * 0x9E3779B1u) >> 22;
    while (slot_of[h] >= 0 && CK[slot_of[h]] != key)
      h = (h + 1) & 1023;
    slot_of[h] < 0 && (CK[slot_of[h] = (int16_t)NC++] = key, 0);
    rid < 1000000 && (R2C[rid] = (uint8_t)slot_of[h], 0);
    p += n + 5;
  }
  CK[UNMATCHED = NC++] = 0x5F0000; // '_' sorts after every [A-Z] code
}

// -------------------------------------------------------------------- parsing

// End of the quoted field whose value starts at s: one past the comma after
// the closing quote, or nullptr on malformed input. *esc counts "" pairs.
// Find the next quote; SSE2 scan with a memchr tail. End-bounded.
static inline const char *find_quote(const char *s, const char *end) {
  __m256i q = _mm256_set1_epi8('"');
  for (; s + 32 <= end; s += 32) {
    int m = _mm256_movemask_epi8(
        _mm256_cmpeq_epi8(_mm256_loadu_si256((const __m256i *)s), q));
    if (m)
      return s + __builtin_ctz(m);
  }
  return (const char *)memchr(s, '"', end - s);
}

static __attribute__((always_inline)) inline const char *
quoted_end(const char *s, const char *end, uint32_t *esc) {
  const char *q = (*esc = 0, find_quote(s, end));
  // "" decodes to one data quote
  while (__builtin_expect(q != nullptr && q[1] == '"', 0))
    ++*esc, q = find_quote(q + 2, end);
  return q ? q + 2 : nullptr;
}

struct Worker {
  const char *start, *end;
  Stats stats[MAX_C];
};

// flush up to n buffered bitsets: prefetch all lines, then set
static inline void bflush(uint32_t *bnid, uint8_t *bc, uint32_t n) {
  for (uint32_t j = 0; j < n; j++)
    __builtin_prefetch(&CB[bc[j]][bnid[j] >> 6]);
  for (uint32_t j = 0; j < n; j++) {
    uint64_t *pw = &CB[bc[j]][bnid[j] >> 6], mask = 1ULL << (bnid[j] & 63);
    if ((__atomic_load_n(pw, __ATOMIC_RELAXED) & mask) == 0)
      __atomic_fetch_or(pw, mask, __ATOMIC_RELAXED);
  }
}

// CR never appears inside a quoted field, so every "\r\n" ends a record and
// chunks cut exactly on record boundaries. Rows have no embedded newlines.
static const char *gst[97];
static uint32_t gnext = 0;

// field-width integer ending at term; consumes the terminator
static inline uint64_t num(const char *&p, char term) {
  uint64_t v = 0;
  while (*p != term)
    v = v * 10 + (*p++ - '0');
  return p++, v;
}

void *parse_chunk(void *arg) {
  Worker *w = (Worker *)arg;
  Stats *stats = w->stats;
  uint32_t bnid[16], nb = 0;
  uint8_t bc[16];
  for (uint32_t g = __atomic_fetch_add(&gnext, 1, __ATOMIC_RELAXED); g < 96;
       g = __atomic_fetch_add(&gnext, 1, __ATOMIC_RELAXED)) {
    const char *p = gst[g], *end = gst[g + 1];
    while (p < end) {
      // skip a UTF-8 BOM before a record, then the id field
      uint64_t id = (p += 3 * ((uint8_t)*p == 0xEF), num(p, ','));

      // name: empty unquoted, or quoted (non-empty names are always quoted)
      uint32_t name_len = 0, esc;
      uint64_t name_hash = 0;
      if (*p == '"') {
        const char *s = ++p, *e = quoted_end(s, end, &esc);
        if (__builtin_expect(e == nullptr, 0))
          break;
        name_len = (uint32_t)(e - 2 - s) - esc, p = e;
        name_hash = (esc == 0 && name_len <= 25) ? hs(s, name_len, fend)
                                                 : hl(s, e - 2);
      } else
        p++;

      uint32_t slot = name_hash & HM;
      __builtin_prefetch(&NT[slot]);
      // note: only its unescaped byte length is needed
      uint32_t note_len = 0;
      if (*p == '"') {
        const char *s = ++p, *e = quoted_end(s, end, &esc);
        if (__builtin_expect(e == nullptr, 0))
          break;
        note_len = (uint32_t)(e - 2 - s) - esc, p = e;
      } else
        p++;

      // region, amount
      uint32_t reg_id = num(p, ',');
      uint8_t c = (reg_id < 1000000) ? R2C[reg_id] : 255;
      __builtin_prefetch(&CB[c == 255 ? 0 : c][slot >> 6]);
      int64_t neg = *p == '-';
      p += neg;
      int64_t cents = num(p, '.') * 100 + (p[0] - '0') * 10 + (p[1] - '0');
      p += 2, cents = neg ? -cents : cents; // two fraction digits consumed
      p += p < end ? 1 + (*p == '\r') : 0; // "\r\n" or "\n", bare at eof

      // join and aggregate
      c = c == 255 ? UNMATCHED : c;
      Stats &s = stats[c];
      s.count++, s.id_sum += id, s.amount_sum += cents;
      s.amount_min = cents < s.amount_min ? cents : s.amount_min;
      s.amount_max = cents > s.amount_max ? cents : s.amount_max;
      s.name_bytes += name_len, s.note_bytes += note_len;
      uint32_t nid = name_len ? intern_name(name_hash, slot) : 0;
      bnid[nb] = nid, bc[nb] = c;
      if (++nb == 16)
        bflush(bnid, bc, 16), nb = 0;
    }
  }
  bflush(bnid, bc, nb);
  return nullptr;
}

// -------------------------------------------------------------------- output

static void pr_money(FILE *f, int64_t c, char sep) {
  c < 0 && (fputc('-', f), c = -c, 0);
  fprintf(f, "%zu.%02u%c", (size_t)(c / 100), (uint32_t)(c % 100), sep);
}

int main(int argc, char **argv) {
  std::string data_dir = argc > 1 ? argv[1] : "/data";
  std::string out_dir = argc > 2 ? argv[2] : "/out";
  // 8MB anon (RW, private); align to 2MB so 6MB of tables sit on 3 huge pages
  char *big = (char *)mmap(0, 8 << 20, 3, 0x22, -1, 0);
  size_t al = ((size_t)big + (2 << 20) - 1) & ~((size_t)(2 << 20) - 1);
  madvise((void *)al, 6 << 20, MADV_HUGEPAGE);
  // NT 1MB, R2C 1MB, CB the rest
  NT = (uint64_t *)al, R2C = (uint8_t *)(al + (1 << 20)),
  CB = (uint64_t(*)[BITS_W])(al + (2 << 20));
  NT[0] = 1; // reserve id 0: real hashes all have bit 63 set
  load_regions(data_dir + "/regions.csv");
  struct stat st;
  int fd = open((data_dir + "/records.csv").c_str(), O_RDONLY);
  fstat(fd, &st);
  const char *data =
      (const char *)mmap(0, st.st_size, PROT_READ, MAP_PRIVATE, fd, 0);
  madvise((void *)data, st.st_size, MADV_SEQUENTIAL),
  madvise((void *)data, st.st_size, MADV_HUGEPAGE), fend = data + st.st_size;

  // Split into 96 granules at exact record boundaries; workers pull granules
  // from an atomic counter, so a slow granule cannot leave a tail idle.
  const int nth = 12;
  gst[0] = (const char *)memchr(data, '\n', st.st_size) + 1, gst[96] = fend;
  size_t total = fend - gst[0];
  for (int i = 1; i < 96; i++) {
    const char *ap = gst[0] + (i * total) / 96,
               *m = (const char *)memmem(ap, fend - ap, "\r\n", 2);
    gst[i] = m ? m + 2 : fend;
  }

  static Worker workers[nth];
  pthread_t ths[nth];
  for (int i = 0; i < nth; i++)
    pthread_create(&ths[i], nullptr, parse_chunk, &workers[i]);
  for (int i = 0; i < nth; i++)
    pthread_join(ths[i], nullptr);

  // Merge the per-thread aggregates (empty threads merge as zeros).
  Stats final_s[MAX_C];
  for (uint32_t i = 0; i < (uint32_t)(nth * NC); i++) {
    const Stats &ws = workers[i / NC].stats[i % NC];
    Stats &fs = final_s[i % NC];
    fs.count += ws.count, fs.id_sum += ws.id_sum, fs.amount_sum += ws.amount_sum;
    fs.amount_min = std::min(fs.amount_min, ws.amount_min);
    fs.amount_max = std::max(fs.amount_max, ws.amount_max);
    fs.name_bytes += ws.name_bytes, fs.note_bytes += ws.note_bytes;
  }

  // Countries sort by their packed 3-byte key; '_' (UNMATCHED) is last.
  uint32_t order[MAX_C];
  for (uint32_t i = 0; i < NC; i++)
    order[i] = i;
  std::sort(order, order + NC, [](auto a, auto b) { return CK[a] < CK[b]; });

  mkdir(out_dir.c_str(), 0777);
  FILE *out = fopen((out_dir + "/aggregates.txt").c_str(), "w");
  if (!out)
    return 1;
  for (uint32_t oi = 0; oi < NC; oi++) {
    uint32_t c = order[oi], d = 0;
    const Stats &g = final_s[c];
    if (g.count == 0)
      continue;
    for (uint32_t i = 0; i < BITS_W; i++)
      d += __builtin_popcountll(CB[c][i]);
    char code[4] = {(char)(CK[c] >> 16), (char)(CK[c] >> 8), (char)CK[c], 0};
    const char *nm = c == UNMATCHED ? "_UNMATCHED" : code;
    fprintf(out, "%s %zu %zu ", nm, (size_t)g.count, (size_t)g.id_sum),
    pr_money(out, g.amount_sum, ' '), pr_money(out, g.amount_min, ' '),
        pr_money(out, g.amount_max, ' ');
    fprintf(out, "%u %zu %zu\n", d, (size_t)g.name_bytes, (size_t)g.note_bytes);
  }
  return fclose(out), 0;
}
