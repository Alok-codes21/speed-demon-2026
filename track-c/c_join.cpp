#include <iostream>
#include <fstream>
#include <vector>
#include <string>
#include <algorithm>
#include <cstring>
#include <cstdint>

#ifdef _WIN32
#include <windows.h>
#else
#include <sys/mman.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <pthread.h>
#endif

// Fast 64-bit word hash
static inline uint64_t hash_bytes(const char* s, size_t len) {
    uint64_t h = 0x517cc1b727220a95ULL ^ (len * 0x9e3779b97f4a7c15ULL);
    for (size_t i = 0; i < len; ++i) {
        h = (h ^ (uint8_t)s[i]) * 0x100000001b3ULL;
    }
    return h | (1ULL << 63);
}

const uint32_t HASH_SIZE = 262144, HASH_MASK = HASH_SIZE - 1, MAX_C = 256, BITS_W = 2500;

struct Slot {
    uint64_t key;
    uint32_t id;
    uint32_t len;
    char str[28];
};

static Slot name_table[HASH_SIZE];
static uint32_t total_names = 1;

static inline uint32_t intern_name(const char* str, uint32_t len) {
    if (len == 0) return 0;
    uint64_t h = hash_bytes(str, len);
    uint32_t idx = h & HASH_MASK;
    while (true) {
        uint64_t k = __atomic_load_n(&name_table[idx].key, __ATOMIC_ACQUIRE);
        if (k == 0) {
            uint64_t zero = 0;
            if (__atomic_compare_exchange_n(&name_table[idx].key, &zero, 1ULL, false, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
                uint32_t id = __atomic_fetch_add(&total_names, 1, __ATOMIC_RELAXED);
                name_table[idx].id = id;
                name_table[idx].len = len;
                memcpy(name_table[idx].str, str, len);
                __atomic_store_n(&name_table[idx].key, h, __ATOMIC_RELEASE);
                return id;
            }
            k = __atomic_load_n(&name_table[idx].key, __ATOMIC_ACQUIRE);
        }
        while (k == 1ULL) k = __atomic_load_n(&name_table[idx].key, __ATOMIC_ACQUIRE);
        if (k == h && name_table[idx].len == len && memcmp(name_table[idx].str, str, len) == 0)
            return name_table[idx].id;
        idx = (idx + 1) & HASH_MASK;
    }
}

struct Stats {
    uint64_t count = 0, id_sum = 0, name_bytes = 0, note_bytes = 0;
    int64_t amount_sum = 0, amount_min = INT64_MAX, amount_max = INT64_MIN;
};

static uint64_t country_bits[MAX_C][BITS_W];
static uint8_t region_to_country[1000000];
static std::vector<std::string> country_names;
static uint8_t UNMATCHED_ID = 255;

void load_regions(const std::string& path) {
    std::ifstream f(path, std::ios::binary);
    std::string line;
    if (!std::getline(f, line)) return;
    std::vector<std::pair<uint32_t, std::string>> entries;
    std::vector<std::string> raw;
    while (std::getline(f, line)) {
        if (line.empty() || line == "\r") continue;
        size_t comma = line.find(',');
        if (comma == std::string::npos) continue;
        uint32_t rid = 0;
        for (size_t i = 0; i < comma; ++i) rid = rid * 10 + (line[i] - '0');
        std::string country = line.substr(comma + 1);
        while (!country.empty() && (country.back() <= ' ')) country.pop_back();
        entries.push_back({rid, country});
        raw.push_back(country);
    }
    std::sort(raw.begin(), raw.end());
    raw.erase(std::unique(raw.begin(), raw.end()), raw.end());
    country_names = raw;
    UNMATCHED_ID = (uint8_t)country_names.size();
    country_names.push_back("_UNMATCHED");
    std::memset(region_to_country, UNMATCHED_ID, sizeof(region_to_country));
    for (const auto& e : entries) {
        if (e.first < 1000000) {
            auto it = std::lower_bound(raw.begin(), raw.end(), e.second);
            region_to_country[e.first] = (uint8_t)(it - raw.begin());
        }
    }
}

struct Worker {
    const char *start, *end;
    Stats stats[MAX_C];
};

void parse_chunk(Worker* w) {
    const char *p = w->start, *end = w->end;
    Stats* stats = w->stats;
    char unesc[64];
    while (p < end) {
        if (p + 3 <= end && (uint8_t)p[0] == 0xEF && (uint8_t)p[1] == 0xBB && (uint8_t)p[2] == 0xBF) p += 3;
        uint64_t id = 0;
        while (*p != ',') id = id * 10 + (*p++ - '0');
        p++;

        const char* name_ptr = "";
        uint32_t name_len = 0;
        if (*p == '"') {
            const char* start = ++p;
            const char* q = (const char*)memchr(p, '"', end - p);
            if (q && q[1] != '"') {
                name_ptr = start;
                name_len = q - start;
                p = q + 2;
            } else {
                bool esc = false;
                while (true) {
                    if (*p == '"') {
                        if (*(p + 1) == '"') { esc = true; p += 2; }
                        else break;
                    } else p++;
                }
                if (!esc) { name_ptr = start; name_len = p - start; }
                else {
                    uint32_t l = 0;
                    for (const char* s = start; s < p; ) {
                        if (*s == '"' && *(s + 1) == '"') { unesc[l++] = '"'; s += 2; }
                        else unesc[l++] = *s++;
                    }
                    name_ptr = unesc;
                    name_len = l;
                }
                p += 2;
            }
        } else p++;

        uint32_t note_len = 0;
        if (*p == '"') {
            const char* start = ++p;
            const char* q = (const char*)memchr(p, '"', end - p);
            if (q && q[1] != '"') {
                note_len = q - start;
                p = q + 2;
            } else {
                uint32_t esc_c = 0;
                while (true) {
                    if (*p == '"') {
                        if (*(p + 1) == '"') { esc_c++; p += 2; }
                        else break;
                    } else p++;
                }
                note_len = (p - start) - esc_c;
                p += 2;
            }
        } else p++;

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

        uint8_t c = (reg_id < 1000000) ? region_to_country[reg_id] : UNMATCHED_ID;
        Stats& s = stats[c];
        s.count++;
        s.id_sum += id;
        s.amount_sum += cents;
        if (cents < s.amount_min) s.amount_min = cents;
        if (cents > s.amount_max) s.amount_max = cents;
        s.name_bytes += name_len;
        s.note_bytes += note_len;

        uint32_t nid = intern_name(name_ptr, name_len);
        uint64_t mask = 1ULL << (nid & 63);
        uint64_t* pw = &country_bits[c][nid >> 6];
        if ((*pw & mask) == 0) __atomic_fetch_or(pw, mask, __ATOMIC_RELAXED);
    }
}

#ifdef _WIN32
DWORD WINAPI th_entry(LPVOID a) { parse_chunk((Worker*)a); return 0; }
#else
void* th_entry(void* a) { parse_chunk((Worker*)a); return nullptr; }
#endif

static inline char* write_u64(char* p, uint64_t v) {
    if (v == 0) { *p++ = '0'; return p; }
    char b[24]; int n = 0;
    while (v > 0) { b[n++] = '0' + (v % 10); v /= 10; }
    for (int i = n - 1; i >= 0; --i) *p++ = b[i];
    return p;
}

static inline char* write_cents(char* p, int64_t c) {
    if (c < 0) { *p++ = '-'; c = -c; }
    uint64_t w = c / 100; uint32_t f = c % 100;
    p = write_u64(p, w);
    *p++ = '.';
    *p++ = '0' + (f / 10);
    *p++ = '0' + (f % 10);
    return p;
}

int main(int argc, char** argv) {
    std::string data_dir = (argc > 1) ? argv[1] : "/data";
    std::string out_dir = (argc > 2) ? argv[2] : "/out";
    load_regions(data_dir + "/regions.csv");

    std::string rec_path = data_dir + "/records.csv";
#ifdef _WIN32
    HANDLE hf = CreateFileA(rec_path.c_str(), GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, NULL);
    if (hf == INVALID_HANDLE_VALUE) return 1;
    LARGE_INTEGER li; GetFileSizeEx(hf, &li); size_t file_sz = li.QuadPart;
    HANDLE hm = CreateFileMappingA(hf, NULL, PAGE_READONLY, 0, 0, NULL);
    const char* data = (const char*)MapViewOfFile(hm, FILE_MAP_READ, 0, 0, file_sz);
#else
    int fd = open(rec_path.c_str(), O_RDONLY);
    if (fd < 0) return 1;
    struct stat st; fstat(fd, &st); size_t file_sz = st.st_size;
    const char* data = (const char*)mmap(nullptr, file_sz, PROT_READ, MAP_PRIVATE, fd, 0);
    madvise((void*)data, file_sz, MADV_SEQUENTIAL);
#endif

    const char *p = data, *end = data + file_sz;
    if (p + 3 <= end && (uint8_t)p[0] == 0xEF && (uint8_t)p[1] == 0xBB && (uint8_t)p[2] == 0xBF) p += 3;
    while (p < end && *p != '\n') p++;
    if (p < end && *p == '\n') p++;

    const char* data_start = p;
    int nth = 12;
#ifdef _WIN32
    SYSTEM_INFO si; GetSystemInfo(&si);
    if (si.dwNumberOfProcessors > 0 && si.dwNumberOfProcessors < 12) nth = si.dwNumberOfProcessors;
#endif

    std::vector<const char*> c_starts(nth + 1);
    c_starts[0] = data_start;
    c_starts[nth] = end;
    size_t total = end - data_start;
    for (int i = 1; i < nth; ++i) {
        const char* ap = data_start + (i * total) / nth;
        while (ap + 1 < end) {
            if (ap[0] == '\r' && ap[1] == '\n') { ap += 2; break; }
            ap++;
        }
        c_starts[i] = ap;
    }

    std::vector<Worker> workers(nth);
#ifdef _WIN32
    std::vector<HANDLE> ths(nth);
    for (int i = 0; i < nth; ++i) {
        workers[i].start = c_starts[i]; workers[i].end = c_starts[i + 1];
        ths[i] = CreateThread(NULL, 0, th_entry, &workers[i], 0, NULL);
    }
    WaitForMultipleObjects(nth, ths.data(), TRUE, INFINITE);
    for (int i = 0; i < nth; ++i) CloseHandle(ths[i]);
#else
    std::vector<pthread_t> ths(nth);
    for (int i = 0; i < nth; ++i) {
        workers[i].start = c_starts[i]; workers[i].end = c_starts[i + 1];
        pthread_create(&ths[i], nullptr, th_entry, &workers[i]);
    }
    for (int i = 0; i < nth; ++i) pthread_join(ths[i], nullptr);
#endif

    Stats final_s[MAX_C];
    for (int i = 0; i < nth; ++i) {
        for (size_t c = 0; c < country_names.size(); ++c) {
            const Stats& ws = workers[i].stats[c];
            if (ws.count == 0) continue;
            Stats& fs = final_s[c];
            fs.count += ws.count; fs.id_sum += ws.id_sum; fs.amount_sum += ws.amount_sum;
            if (ws.amount_min < fs.amount_min) fs.amount_min = ws.amount_min;
            if (ws.amount_max > fs.amount_max) fs.amount_max = ws.amount_max;
            fs.name_bytes += ws.name_bytes; fs.note_bytes += ws.note_bytes;
        }
    }

#ifdef _WIN32
    CreateDirectoryA(out_dir.c_str(), NULL);
#else
    mkdir(out_dir.c_str(), 0777);
#endif
    std::ofstream out_f(out_dir + "/aggregates.txt", std::ios::binary);
    char buf[512];
    for (size_t c = 0; c < country_names.size(); ++c) {
        const Stats& g = final_s[c];
        if (g.count == 0) continue;
        uint32_t dist = 0;
        for (uint32_t w = 0; w < BITS_W; ++w) {
            if (country_bits[c][w]) dist += __builtin_popcountll(country_bits[c][w]);
        }
        char* pt = buf;
        const std::string& nm = country_names[c];
        memcpy(pt, nm.data(), nm.size()); pt += nm.size(); *pt++ = ' ';
        pt = write_u64(pt, g.count); *pt++ = ' ';
        pt = write_u64(pt, g.id_sum); *pt++ = ' ';
        pt = write_cents(pt, g.amount_sum); *pt++ = ' ';
        pt = write_cents(pt, g.amount_min); *pt++ = ' ';
        pt = write_cents(pt, g.amount_max); *pt++ = ' ';
        pt = write_u64(pt, dist); *pt++ = ' ';
        pt = write_u64(pt, g.name_bytes); *pt++ = ' ';
        pt = write_u64(pt, g.note_bytes); *pt++ = '\n';
        out_f.write(buf, pt - buf);
    }
    return 0;
}
