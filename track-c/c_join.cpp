// Speed Demon 2026, Track C (Join). Linux, C++20, no dependencies.
//
// mmap records.csv -> 12 threads parse disjoint chunks at record boundaries
// -> merge per-thread aggregates -> one buffered write. Names are interned
// to small ids in a lock-free hash table; each country keeps a bitset over
// name ids for the distinct count.

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <string>
#include <vector>
#include <fcntl.h>
#include <pthread.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

static const uint32_t HASH_SIZE = 262144, HASH_MASK = HASH_SIZE - 1;
static const uint32_t MAX_C = 256, BITS_W = 1600;  // country slots, 64-bit words per bitset
static const uint32_t CACHE_SIZE = 4096, CACHE_MASK = CACHE_SIZE - 1;
static const char* fend;                           // end of the records.csv mapping

struct Stats {
    uint64_t count = 0, id_sum = 0, name_bytes = 0, note_bytes = 0;
    int64_t amount_sum = 0, amount_min = INT64_MAX, amount_max = INT64_MIN;
};

static uint64_t country_bits[MAX_C][BITS_W];
static uint8_t region_to_country[1000000];
static uint32_t country_key[MAX_C];  // sorted packed 3-letter country codes
static uint32_t n_countries = 0, UNMATCHED = 0;

struct Slot { uint64_t key; uint32_t id, pad; };
static Slot name_table[HASH_SIZE];
static uint32_t total_names = 1;  // id 0 is the empty name
struct CacheEntry { uint64_t key; uint32_t id; };

static inline uint32_t intern_name(CacheEntry* cache, uint64_t h) {
    uint32_t c_idx = (h & CACHE_MASK) & ~1u;
    if (cache[c_idx].key == h) return cache[c_idx].id;
    if (cache[c_idx + 1].key == h) return cache[c_idx + 1].id;
    uint32_t idx = h & HASH_MASK;
    while (true) {
        uint64_t k = __atomic_load_n(&name_table[idx].key, __ATOMIC_ACQUIRE);
        if (k == 0) {
            uint64_t zero = 0;
            if (__atomic_compare_exchange_n(&name_table[idx].key, &zero, 1ULL, false, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
                uint32_t id = __atomic_fetch_add(&total_names, 1, __ATOMIC_RELAXED);
                name_table[idx].id = id;
                __atomic_store_n(&name_table[idx].key, h, __ATOMIC_RELEASE);
                cache[c_idx + (id & 1)] = {h, id};
                return id;
            }
            k = __atomic_load_n(&name_table[idx].key, __ATOMIC_ACQUIRE);
        }
        while (k == 1ULL) k = __atomic_load_n(&name_table[idx].key, __ATOMIC_ACQUIRE);
        if (k == h) {
            cache[c_idx + (name_table[idx].id & 1)] = {h, name_table[idx].id};
            return name_table[idx].id;
        }
        idx = (idx + 1) & HASH_MASK;
    }
}

// 8 bytes with the high (8 - n) bytes zeroed, never reading past end.
static inline uint64_t load8(const char* s, uint32_t n, const char* end) {
    if (__builtin_expect(s + 8 <= end, 1)) {
        uint64_t v;
        memcpy(&v, s, 8);
        return n < 8 ? v & ((1ULL << (8 * n)) - 1) : v;
    }
    uint64_t v = 0;
    for (uint32_t i = 0; i < n; i++) v |= (uint64_t)(uint8_t)s[i] << (8 * i);
    return v;
}

// Fast path: a name of at most 25 bytes packs into four 64-bit words.
static inline uint64_t hash_short(const char* s, uint32_t len, const char* end) {
    uint64_t w0 = load8(s, len < 8 ? len : 8, end);
    uint64_t w1 = len > 8 ? load8(s + 8, len - 8 < 8 ? len - 8 : 8, end) : 0;
    uint64_t w2 = len > 16 ? load8(s + 16, len - 16 < 8 ? len - 16 : 8, end) : 0;
    uint64_t w3 = len > 24 ? (uint8_t)s[24] : 0;
    uint64_t h = w0 ^ (w1 * 0x9e3779b97f4a7c15ULL) ^ (w2 * 0x517cc1b727220a95ULL) ^ (w3 * 0x85ebca6bULL) ^ ((uint64_t)len * 0xbf58476d1ce4e5b9ULL);
    h ^= h >> 32; h *= 0xd6e8feb86659fd93ULL; h ^= h >> 32;
    return h | (1ULL << 63);
}

// Slow path (rare): names over 25 bytes get a full-length FNV-1a hash, so
// distinct long names can never be merged by truncation. "" is one quote.
static uint64_t hash_long(const char* s, const char* e) {
    uint64_t h = 1469598103934665603ULL;
    while (s < e) {
        uint8_t c = (uint8_t)*s++;
        if (c == '"' && s < e && *s == '"') s++;
        h = (h ^ c) * 1099511628211ULL;
    }
    return h | (1ULL << 63);
}

// ---------------------------------------------------------------- regions.csv

void load_regions(const std::string& path) {
    int fd = open(path.c_str(), O_RDONLY);
    if (fd < 0) return;
    struct stat st;
    fstat(fd, &st);
    const char* data = (const char*)mmap(nullptr, st.st_size, PROT_READ, MAP_PRIVATE, fd, 0);
    const char *p = data, *end = data + st.st_size;
    if (p < end) {  // skip the header line
        p = (const char*)memchr(p, '\n', end - p);
        p = p ? p + 1 : end;
    }
    // First pass: intern each 3-letter code by first appearance.
    uint32_t keys[256];
    int16_t slot_of[1024];
    memset(slot_of, -1, sizeof(slot_of));
    memset(region_to_country, 255, sizeof(region_to_country));  // 255 = not in table
    while (p < end) {
        uint32_t rid = 0;
        while (*p != ',') rid = rid * 10 + (*p++ - '0');
        p++;
        uint32_t key = ((uint8_t)p[0] << 16) | ((uint8_t)p[1] << 8) | (uint8_t)p[2];
        uint32_t h = (key * 0x9E3779B1u) >> 22;
        while (slot_of[h] >= 0 && keys[slot_of[h]] != key) h = (h + 1) & 1023;
        if (slot_of[h] < 0) {
            slot_of[h] = (int16_t)n_countries;
            keys[n_countries++] = key;
        }
        if (rid < 1000000) region_to_country[rid] = (uint8_t)slot_of[h];
        p += 3;
        if (p < end && *p == '\n') p++;
    }
    // Sort the codes, then remap the table to sorted positions in one pass.
    uint32_t sorted[256];
    memcpy(sorted, keys, n_countries * 4);
    std::sort(sorted, sorted + n_countries);
    uint8_t remap[256];
    for (uint32_t i = 0; i < n_countries; i++)
        remap[i] = (uint8_t)(std::lower_bound(sorted, sorted + n_countries, keys[i]) - sorted);
    for (uint32_t i = 0; i < 1000000; i++)
        region_to_country[i] = region_to_country[i] == 255 ? 255 : remap[region_to_country[i]];
    memcpy(country_key, sorted, n_countries * 4);
    UNMATCHED = n_countries;
    munmap((void*)data, st.st_size);
    close(fd);
}

// -------------------------------------------------------------------- parsing


// Find the next quote; SSE2 scan with a memchr tail. End-bounded.
#include <emmintrin.h>
static inline const char* find_quote(const char* s, const char* end) {
    __m128i q = _mm_set1_epi8('"');
    while (s + 16 <= end) {
        int m = _mm_movemask_epi8(_mm_cmpeq_epi8(_mm_loadu_si128((const __m128i*)s), q));
        if (m) return s + __builtin_ctz(m);
        s += 16;
    }
    return (const char*)memchr(s, '"', end - s);
}


// Find the end of a quoted field whose value starts at s. Returns one past
// the comma that follows the closing quote; *esc gets the number of "" escape
// pairs inside. nullptr on malformed input.
static inline const char* quoted_end(const char* s, const char* end, uint32_t* esc) {
    const char* q = find_quote(s, end);
    if (__builtin_expect(q == nullptr, 0)) return nullptr;
    if (q[1] != '"') { *esc = 0; return q + 2; }  // no escaped quote: common case
    const char* c = q + 2;
    *esc = 1;
    while (true) {
        c = find_quote(c, end);
        if (__builtin_expect(c == nullptr, 0)) return nullptr;
        if (c[1] == '"') { (*esc)++; c += 2; }
        else return c + 2;
    }
}

struct Worker {
    const char *start, *end;
    Stats stats[MAX_C];
    CacheEntry cache[CACHE_SIZE];
};


void parse_chunk(Worker* w) {
    const char *p = w->start, *end = w->end;
    Stats* stats = w->stats;
    CacheEntry* cache = w->cache;

    while (p < end) {
        if (__builtin_expect((uint8_t)*p == 0xEF, 0)) p += 3;  // BOM before a record
        uint64_t id = 0;
        while (*p != ',') id = id * 10 + (*p++ - '0');
        p++;

        // ---- name: empty unquoted, or quoted (non-empty names are always quoted)
        uint32_t name_len = 0;
        uint64_t name_hash = 0;
        if (*p == '"') {
            const char* s = ++p;
            uint32_t esc;
            const char* e = quoted_end(s, end, &esc);
            if (__builtin_expect(e == nullptr, 0)) break;
            const char* c = e - 2;  // the closing quote
            name_len = (c - s) - esc;
            if (name_len > 25) {
                name_hash = hash_long(s, c);  // long names: full-length hash, no truncation
            } else if (esc == 0) {
                name_hash = hash_short(s, name_len, fend);
            } else {  // unescape into a buffer, then the fast hash
                char buf[64];  // name_len <= 25 means the raw span is <= 50 bytes
                uint32_t l = 0;
                for (const char* t = s; t < c;) {
                    if (*t == '"') { buf[l++] = '"'; t += 2; }
                    else buf[l++] = *t++;
                }
                name_hash = hash_short(buf, l, buf + sizeof(buf));
            }
            p = e;
        } else p++;

        // ---- note: only its unescaped byte length is needed
        uint32_t note_len = 0;
        if (*p == '"') {
            const char* s = ++p;
            uint32_t esc;
            const char* e = quoted_end(s, end, &esc);
            if (__builtin_expect(e == nullptr, 0)) break;
            note_len = (e - 2 - s) - esc;
            p = e;
        } else p++;

        // ---- region, amount
        uint32_t reg_id = 0;
        while (*p != ',') reg_id = reg_id * 10 + (*p++ - '0');
        p++;
        bool neg = (*p == '-');
        if (neg) p++;
        int64_t cents = 0;
        while (*p != '.') cents = cents * 10 + (*p++ - '0');
        p++;
        cents = cents * 100 + (p[0] - '0') * 10 + (p[1] - '0');
        p += 2;
        if (neg) cents = -cents;
        if (p < end && *p == '\r') p++;
        if (p < end && *p == '\n') p++;

        // ---- join and aggregate
        uint8_t c = (reg_id < 1000000) ? region_to_country[reg_id] : 255;
        if (__builtin_expect(c == 255, 0)) c = UNMATCHED;
        Stats& s = stats[c];
        s.count++; s.id_sum += id; s.amount_sum += cents;
        if (cents < s.amount_min) s.amount_min = cents;
        if (cents > s.amount_max) s.amount_max = cents;
        s.name_bytes += name_len; s.note_bytes += note_len;
        uint32_t nid = name_len ? intern_name(cache, name_hash) : 0;
        uint64_t mask = 1ULL << (nid & 63);
        uint64_t* pw = &country_bits[c][nid >> 6];
        if ((*pw & mask) == 0) __atomic_fetch_or(pw, mask, __ATOMIC_RELAXED);
    }
}

void* th_entry(void* a) {
    parse_chunk((Worker*)a);
    return nullptr;
}

// -------------------------------------------------------------------- output

static inline char* write_u64(char* p, uint64_t v) {
    if (v == 0) { *p++ = '0'; return p; }
    char b[24];
    int n = 0;
    while (v > 0) { b[n++] = '0' + (v % 10); v /= 10; }
    while (n > 0) *p++ = b[--n];
    return p;
}

static inline char* write_cents(char* p, int64_t c) {
    if (c < 0) { *p++ = '-'; c = -c; }
    p = write_u64(p, (uint64_t)(c / 100));
    *p++ = '.';
    uint32_t f = c % 100;
    *p++ = '0' + (f / 10);
    *p++ = '0' + (f % 10);
    return p;
}

int main(int argc, char** argv) {
    std::string data_dir = argc > 1 ? argv[1] : "/data";
    std::string out_dir = argc > 2 ? argv[2] : "/out";
    load_regions(data_dir + "/regions.csv");

    int fd = open((data_dir + "/records.csv").c_str(), O_RDONLY);
    if (fd < 0) return 1;
    struct stat st;
    fstat(fd, &st);
    size_t file_sz = st.st_size;
    const char* data = (const char*)mmap(nullptr, file_sz, PROT_READ, MAP_PRIVATE, fd, 0);
    madvise((void*)data, file_sz, MADV_SEQUENTIAL);
    fend = data + file_sz;

    const char* p = data;  // skip the header line
    if (p < fend) {
        p = (const char*)memchr(p, '\n', fend - p);
        p = p ? p + 1 : fend;
    }

    // Split into 12 chunks at exact record boundaries (CR never appears
    // inside a quoted field, so every "\r\n" is a record terminator).
    const int nth = 12;
    const char* c_starts[nth + 1];
    c_starts[0] = p;
    c_starts[nth] = fend;
    size_t total = fend - p;
    for (int i = 1; i < nth; i++) {
        const char* ap = p + (i * total) / nth;
        while (ap + 1 < fend) {
            if (ap[0] == '\r' && ap[1] == '\n') { ap += 2; break; }
            ap++;
        }
        c_starts[i] = ap;
    }

    std::vector<Worker> workers(nth);
    pthread_t ths[nth];
    for (int i = 0; i < nth; i++) {
        workers[i].start = c_starts[i];
        workers[i].end = c_starts[i + 1];
        pthread_create(&ths[i], nullptr, th_entry, &workers[i]);
    }
    for (int i = 0; i < nth; i++) pthread_join(ths[i], nullptr);

    Stats final_s[MAX_C];
    for (int i = 0; i < nth; i++)
        for (uint32_t c = 0; c <= n_countries; c++) {
            const Stats& ws = workers[i].stats[c];
            if (ws.count == 0) continue;
            Stats& fs = final_s[c];
            fs.count += ws.count; fs.id_sum += ws.id_sum; fs.amount_sum += ws.amount_sum;
            if (ws.amount_min < fs.amount_min) fs.amount_min = ws.amount_min;
            if (ws.amount_max > fs.amount_max) fs.amount_max = ws.amount_max;
            fs.name_bytes += ws.name_bytes; fs.note_bytes += ws.note_bytes;
        }

    mkdir(out_dir.c_str(), 0777);
    char out_buf[65536];
    char* pt = out_buf;
    for (uint32_t c = 0; c <= n_countries; c++) {
        const Stats& g = final_s[c];
        if (g.count == 0) continue;
        uint32_t dist = 0;
        for (uint32_t i = 0; i < BITS_W; i++) dist += __builtin_popcountll(country_bits[c][i]);
        if (c == n_countries) {
            memcpy(pt, "_UNMATCHED", 10);
            pt += 10;
        } else {
            uint32_t k = country_key[c];
            *pt++ = (k >> 16) & 0xFF; *pt++ = (k >> 8) & 0xFF; *pt++ = k & 0xFF;
        }
        *pt++ = ' ';
        pt = write_u64(pt, g.count); *pt++ = ' ';
        pt = write_u64(pt, g.id_sum); *pt++ = ' ';
        pt = write_cents(pt, g.amount_sum); *pt++ = ' ';
        pt = write_cents(pt, g.amount_min); *pt++ = ' ';
        pt = write_cents(pt, g.amount_max); *pt++ = ' ';
        pt = write_u64(pt, dist); *pt++ = ' ';
        pt = write_u64(pt, g.name_bytes); *pt++ = ' ';
        pt = write_u64(pt, g.note_bytes); *pt++ = '\n';
    }
    int out_fd = open((out_dir + "/aggregates.txt").c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (out_fd < 0) return 1;
    const char* b = out_buf;
    size_t left = pt - out_buf;
    while (left > 0) {
        ssize_t w = write(out_fd, b, left);
        if (w <= 0) break;
        b += w;
        left -= w;
    }
    close(out_fd);
    return 0;
}
