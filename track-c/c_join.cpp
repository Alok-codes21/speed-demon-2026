// Speed Demon 2026, Track C (Join). Linux, C++20, no dependencies.
//
// mmap records.csv -> 12 threads parse disjoint chunks at record boundaries
// -> merge per-thread aggregates -> one buffered write. Names are interned
// to small ids in a lock-free hash table; each country keeps a bitset over
// name ids for the distinct count.

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <emmintrin.h>
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
static inline uint32_t intern_name(uint64_t h) {
  for (uint32_t idx = h & HM;; idx = (idx + 1) & HM) {
    uint64_t k = __atomic_load_n(&NT[idx], __ATOMIC_ACQUIRE);
    if (k == 0)
      k = __sync_val_compare_and_swap(&NT[idx], 0, h);
    if (k == h || k == 0) return idx; // k==0: the CAS above installed h
  }
}

// 8 bytes with the high (8 - n) bytes zeroed, never reading past end.
static inline uint64_t load8(const char *s, uint32_t n, const char *end) {
  uint64_t v = 0;
  if (__builtin_expect(s + 8 <= end, 1))
    memcpy(&v, s, 8);
  else {
    char tmp[8] = {}; memcpy(tmp, s, n); memcpy(&v, tmp, 8);
  }
  return n < 8 ? v & ((1ULL << (8 * n)) - 1) : v;
}

// Fast path: a name of at most 25 bytes packs into four 64-bit words.
static inline uint64_t hs(const char *s, uint32_t len, const char *end) {
  uint64_t w0 = load8(s, len < 8 ? len : 8, end);
  uint64_t w1 = len > 8 ? load8(s + 8, len - 8 < 8 ? len - 8 : 8, end) : 0;
  uint64_t w2 = len > 16 ? load8(s + 16, len - 16 < 8 ? len - 16 : 8, end) : 0;
  uint64_t w3 = len > 24 ? (uint8_t)s[24] : 0;
  uint64_t h = w0 ^ (w1 * 0x9e3779b97f4a7c15ULL) ^ (w2 * 0x517cc1b727220a95ULL);
  h ^= (w3 * 0x85ebca6bULL) ^ ((uint64_t)len * 0xbf58476d1ce4e5b9ULL);
  h ^= h >> 32;
  h *= 0xd6e8feb86659fd93ULL;
  h ^= h >> 32;
  return h | (1ULL << 63);
}

// Everything else (escaped or over 25 bytes): full-length FNV-1a over the
// unescaped value, so distinct names are never merged by truncation and no
// unescape buffer is needed. "" decodes to one quote. Rare on real data.
static uint64_t hash_long(const char *s, const char *e) {
  uint64_t h = 1469598103934665603ULL;
  while (s < e) h = (h ^ (uint8_t)*s++) * 1099511628211ULL;
  return h | (1ULL << 63);
}

// ---------------------------------------------------------------- regions.csv

void load_regions(const std::string &path) {
  int fd = open(path.c_str(), O_RDONLY);
  struct stat st; fstat(fd, &st);
  auto data = (const char *)mmap(0, st.st_size, PROT_READ, MAP_PRIVATE, fd, 0);
  const char *p = data, *end = data + st.st_size;
  p = (const char *)memchr(p, '\n', end - p) + 1; // skip the header line
  int16_t slot_of[1024]; // country code -> slot, by first appearance
  memset(slot_of, -1, sizeof(slot_of));
  memset(R2C, 255, 1000000);
  while (p < end) {
    // SWAR rid parse: comma index = digit count, branch-free convert
    uint64_t rid = 0, n = 0, zm = 0;
    if (p + 16 <= end) {
      uint64_t v, x; memcpy(&v, p, 8); x = v ^ 0x2C2C2C2C2C2C2C2CULL;
      zm = (x - 0x0101010101010101ULL) & ~x & 0x8080808080808080ULL;
      if (zm) n = __builtin_ctzll(zm) >> 3,
        v <<= 64 - 8 * n, // left-align digits in the 8 lanes
        v -= 0x3030303030303030ULL & (~0ULL << (64 - 8 * n)),
        v = v * 2561 >> 8,
        v = (v & 0x00FF00FF00FF00FFULL) * 6553601 >> 16,
        rid = (uint32_t)((v & 0x0000FFFF0000FFFFULL) * 42949672960001ULL >> 32);
    }
    while (zm == 0 && p[n] != ',') rid = rid * 10 + (p[n++] - '0'); // tail
    uint32_t w, key = (memcpy(&w, p + n + 1, 4), __builtin_bswap32(w) >> 8);
    uint32_t h = (key * 0x9E3779B1u) >> 22;
    while (slot_of[h] >= 0 && CK[slot_of[h]] != key) h = (h + 1) & 1023;
    if (slot_of[h] < 0) CK[slot_of[h] = (int16_t)NC++] = key;
    if (rid < 1000000) R2C[rid] = (uint8_t)slot_of[h];
    p += n + 5;
  }
  CK[UNMATCHED = NC++] = 0x5F0000; // '_' sorts after every [A-Z] code
  munmap((void *)data, st.st_size);
  close(fd);
}

// -------------------------------------------------------------------- parsing

// End of the quoted field whose value starts at s: one past the comma after
// the closing quote, or nullptr on malformed input. *esc counts "" pairs.
// Find the next quote; SSE2 scan with a memchr tail. End-bounded.
static inline const char *find_quote(const char *s, const char *end) {
  __m256i q = _mm256_set1_epi8('"');
  while (s + 32 <= end) {
    int m = _mm256_movemask_epi8(
        _mm256_cmpeq_epi8(_mm256_loadu_si256((const __m256i *)s), q));
    if (m) return s + __builtin_ctz(m);
    s += 32;
  }
  return (const char *)memchr(s, '"', end - s);
}

static __attribute__((always_inline)) inline const char *
quoted_end(const char *s, const char *end, uint32_t *esc) {
  const char *q = (*esc = 0, find_quote(s, end));
  // "" decodes to one data quote
  while (__builtin_expect(q != nullptr && q[1] == '"', 0)) {
    (*esc)++;
    q = find_quote(q + 2, end);
  }
  return q ? q + 2 : nullptr;
}

struct Worker {
  const char *start, *end; Stats stats[MAX_C];
};

// flush up to n buffered bitsets: prefetch all lines, then set
static inline void bflush(uint32_t *bnid, uint8_t *bc, uint32_t n) {
  for (uint32_t j = 0; j < n; j++) __builtin_prefetch(&CB[bc[j]][bnid[j] >> 6]);
  for (uint32_t j = 0; j < n; j++) {
    uint64_t *pw = &CB[bc[j]][bnid[j] >> 6], mask = 1ULL << (bnid[j] & 63);
    if ((__atomic_load_n(pw, __ATOMIC_RELAXED) & mask) == 0)
      __atomic_fetch_or(pw, mask, __ATOMIC_RELAXED);
  }
}

// CR never appears inside a quoted field, so every "\r\n" ends a record and
// chunks cut exactly on record boundaries. Rows have no embedded newlines.
void *parse_chunk(void *arg) {
  Worker *w = (Worker *)arg;
  const char *p = w->start, *end = w->end;
  Stats *stats = w->stats;
  uint32_t bnid[16], nb = 0; uint8_t bc[16];
  while (p < end) {
    p += 3 * ((uint8_t)*p == 0xEF); // skip a UTF-8 BOM before a record
    uint64_t id = 0; while (*p != ',') id = id * 10 + (*p++ - '0'); p++;

    // name: empty unquoted, or quoted (non-empty names are always quoted)
    uint32_t name_len = 0, esc; uint64_t name_hash = 0;
    if (*p == '"') {
      const char *s = ++p, *e = quoted_end(s, end, &esc);
      if (__builtin_expect(e == nullptr, 0)) break;
      name_len = (uint32_t)(e - 2 - s) - esc; // e[-2] is the closing quote
      name_hash = (esc == 0 && name_len <= 25) ? hs(s, name_len, fend)
                                               : hash_long(s, e - 2);
      p = e;
    } else
      p++;

    if (name_len) __builtin_prefetch(&NT[name_hash & HM]);
    // note: only its unescaped byte length is needed
    uint32_t note_len = 0;
    if (*p == '"') {
      const char *s = ++p, *e = quoted_end(s, end, &esc);
      if (__builtin_expect(e == nullptr, 0)) break;
      note_len = (uint32_t)(e - 2 - s) - esc;
      p = e;
    } else
      p++;

    // region, amount
    uint32_t reg_id = 0; while (*p != ',') reg_id = reg_id * 10 + (*p++ - '0'); p++;
    uint8_t c = (reg_id < 1000000) ? R2C[reg_id] : 255;
    if (name_len) __builtin_prefetch(&CB[c][(name_hash & HM) >> 6]);
    bool neg = (*p == '-'); p += neg;
    int64_t cents = 0; while (*p != '.') cents = cents * 10 + (*p++ - '0');
    cents = cents * 100 + (p[1] - '0') * 10 + (p[2] - '0');
    p += 3; // '.' and two fraction digits
    if (neg) cents = -cents;
    p += (p < end && *p == '\r'); p += (p < end && *p == '\n');

    // join and aggregate
    if (__builtin_expect(c == 255, 0)) c = UNMATCHED;
    Stats &s = stats[c]; s.count++; s.id_sum += id; s.amount_sum += cents;
    if (cents < s.amount_min)
      s.amount_min = cents;
    if (cents > s.amount_max)
      s.amount_max = cents;
    s.name_bytes += name_len; s.note_bytes += note_len;
    uint32_t nid = name_len ? intern_name(name_hash) : 0;
    bnid[nb] = nid; bc[nb] = c;
    if (++nb == 16) bflush(bnid, bc, 16), nb = 0;
  }
  bflush(bnid, bc, nb);
  return nullptr;
}

// -------------------------------------------------------------------- output

static void pr_money(FILE *f, int64_t c, char sep) {
  if (c < 0) fputc('-', f), c = -c;
  fprintf(f, "%zu.%02u%c", (size_t)(c / 100), (uint32_t)(c % 100), sep);
}

int main(int argc, char **argv) {
  std::string data_dir = argc > 1 ? argv[1] : "/data";
  std::string out_dir = argc > 2 ? argv[2] : "/out";
  // 8MB anon (RW, private); align to 2MB so 6MB of tables sit on 3 huge pages
  char *big = (char *)mmap(0, 8 << 20, 3, 0x22, -1, 0);
  size_t al = ((size_t)big + (2 << 20) - 1) & ~((size_t)(2 << 20) - 1);
  madvise((void *)al, 6 << 20, MADV_HUGEPAGE);
  NT = (uint64_t *)al;                     // 1 MB
  R2C = (uint8_t *)(al + (1 << 20)); // 1 MB
  CB = (uint64_t(*)[BITS_W])(al + (2 << 20));
  NT[0] = 1; // reserve id 0: real hashes all have bit 63 set
  load_regions(data_dir + "/regions.csv");
  int fd = open((data_dir + "/records.csv").c_str(), O_RDONLY);
  struct stat st; fstat(fd, &st);
  auto data = (const char *)mmap(0, st.st_size, PROT_READ, MAP_PRIVATE, fd, 0);
  madvise((void *)data, st.st_size, MADV_SEQUENTIAL); fend = data + st.st_size;

  // Split into 12 chunks at exact record boundaries.
  const int nth = 12;
  const char *c_starts[nth + 1];
  c_starts[0] = (const char *)memchr(data, '\n', st.st_size) + 1;
  c_starts[nth] = fend;
  size_t total = fend - c_starts[0];
  for (int i = 1; i < nth; i++) {
    const char *ap = c_starts[0] + (i * total) / nth;
    const char *m = (const char *)memmem(ap, fend - ap, "\r\n", 2);
    c_starts[i] = m ? m + 2 : fend;
  }

  static Worker workers[nth];
  pthread_t ths[nth];
  for (int i = 0; i < nth; i++) {
    workers[i].start = c_starts[i]; workers[i].end = c_starts[i + 1];
    pthread_create(&ths[i], nullptr, parse_chunk, &workers[i]);
  }
  for (int i = 0; i < nth; i++)
    pthread_join(ths[i], nullptr);

  // Merge the per-thread aggregates (empty threads merge as zeros).
  Stats final_s[MAX_C];
  for (uint32_t i = 0; i < (uint32_t)(nth * NC); i++) {
      const Stats &ws = workers[i / NC].stats[i % NC];
      Stats &fs = final_s[i % NC];
      fs.count += ws.count; fs.id_sum += ws.id_sum; fs.amount_sum += ws.amount_sum;
      fs.amount_min = std::min(fs.amount_min, ws.amount_min);
      fs.amount_max = std::max(fs.amount_max, ws.amount_max);
      fs.name_bytes += ws.name_bytes; fs.note_bytes += ws.note_bytes;
    }

  // Countries sort by their packed 3-byte key; '_' (UNMATCHED) is last.
  uint32_t order[MAX_C]; for (uint32_t i = 0; i < NC; i++) order[i] = i;
  std::sort(order, order + NC, [](auto a, auto b) { return CK[a] < CK[b]; });

  mkdir(out_dir.c_str(), 0777);
  FILE *out = fopen((out_dir + "/aggregates.txt").c_str(), "w");
  if (!out) return 1;
  for (uint32_t oi = 0; oi < NC; oi++) {
    uint32_t c = order[oi];
    const Stats &g = final_s[c];
    if (g.count == 0) continue;
    uint32_t d = 0;
    for (uint32_t i = 0; i < BITS_W; i++) d += __builtin_popcountll(CB[c][i]);
    char code[4] = {(char)(CK[c] >> 16), (char)(CK[c] >> 8), (char)CK[c], 0};
    const char *nm = c == UNMATCHED ? "_UNMATCHED" : code;
    fprintf(out, "%s %zu %zu ", nm, (size_t)g.count, (size_t)g.id_sum);
    pr_money(out, g.amount_sum, ' ');
    pr_money(out, g.amount_min, ' ');
    pr_money(out, g.amount_max, ' ');
    fprintf(out, "%u %zu %zu\n", d, (size_t)g.name_bytes, (size_t)g.note_bytes);
  }
  fclose(out);
  return 0;
}
