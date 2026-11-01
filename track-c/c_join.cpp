// Speed Demon 2026, Track C (Join).
// Reused code from Cache Me's MIT-licensed entry:
// https://github.com/manusingh090/speed-demon-2026-entry
// Copyright (c) 2026 Hackathon Raptors CIC
// Copyright (c) 2026 the contributors of that repository
// Modifications: AVX2 block-classifying record scanner; original MIT notices retained.
//
// MIT License
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

#include <algorithm>
#include <array>
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fcntl.h>
#include <immintrin.h>
#include <string>
#include <mutex>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <thread>
#include <unistd.h>
#include <vector>

using u8 = uint8_t;
using u32 = uint32_t;
using u64 = uint64_t;
using i64 = int64_t;

template <class T>
T* zeroed(size_t n) {
    void* p = mmap(nullptr, n * sizeof(T), PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) { perror("mmap"); _exit(1); }
    return static_cast<T*>(p);
}

template <class T>
void populate_share(T* p, size_t n, int t, int nthreads) {
    const size_t pages = (n * sizeof(T) + 4095) / 4096;
    const size_t from = pages * t / nthreads, to = pages * (t + 1) / nthreads;
    if (to > from)
        madvise(reinterpret_cast<char*>(p) + from * 4096,
                (to - from) * 4096, MADV_POPULATE_WRITE);
}

const char* map_file(const char* path, size_t& size) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) { perror(path); _exit(1); }
    size = lseek(fd, 0, SEEK_END);
    auto* p = static_cast<char*>(
        mmap(nullptr, size + 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));
    if (p == MAP_FAILED) { perror("mmap"); _exit(1); }
    if (size && mmap(p, size, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0) == MAP_FAILED) {
        perror("mmap"); _exit(1);
    }
    madvise(p, size, MADV_HUGEPAGE);
    close(fd);
    return p;
}

inline u64 load8(const char* p) {
    u64 v;
    memcpy(&v, p, 8);
    return v;
}

inline u64 fold(u64 v, size_t n) {
    v &= 0x0F0F0F0F0F0F0F0Full & (~u64(0) << (8 * (8 - n)));
    v = (v * 10 + (v >> 8)) & 0x00FF00FF00FF00FFull;
    v = (v * 100 + (v >> 16)) & 0x0000FFFF0000FFFFull;
    return (v * 10000 + (v >> 32)) & 0xFFFFFFFFull;
}

inline u64 digits(const char* s, const char* e) { return fold(load8(e - 8), e - s); }

inline u64 money(const char* s, const char* e) {
    const u64 w = load8(e - 8);
    return fold((w & 0xFFFFFFFFFFull) << 8 | (w & 0xFFFF000000000000ull), e - s - 1);
}

inline const char* comma_after(const char* p) {
    const u64 x = load8(p) ^ 0x2C2C2C2C2C2C2C2Cull;
    return p + __builtin_ctzll((x - 0x0101010101010101ull) &
                               ~x & 0x8080808080808080ull) / 8;
}

// One cursor classifies each 32-byte block once. Delimiter masks are consumed
// in byte order; quote and pending-escape state survive block boundaries.
class RecordScanner {
public:
    RecordScanner(const char* begin, const char* end) : pos_(begin), end_(end) {}

    inline const char* position() const { return pos_; }
    inline void skip(size_t n) { pos_ += n; }

    inline bool next(char& delimiter, const char*& at, u32& escaped_pairs) {
        escaped_pairs = 0;
        while (pos_ < end_) {
            if (!block_ || pos_ >= block_ + 32) load_block();

            if (pending_quote_) {
                if (*pos_ == '"') {
                    ++escaped_pairs;
                    ++pos_;
                    pending_quote_ = false;
                    continue;
                }
                in_quotes_ = false;
                pending_quote_ = false;
            }

            const u32 offset = u32(pos_ - block_);
            const u32 tail = 0xFFFFFFFFu << offset;
            const u32 q = quotes_ & tail;
            const u32 c = commas_ & tail;
            const u32 l = lfs_ & tail;
            const u32 events = q | c | l;

            if (!events) {
                pos_ = block_ + 32;
                if (pos_ > end_) pos_ = end_;
                continue;
            }

            const u32 bit = events & (0u - events);
            const u32 i = __builtin_ctz(events);
            const char* event = block_ + i;

            if (q & bit) {
                if (!in_quotes_) {
                    in_quotes_ = true;
                    pos_ = event + 1;
                } else if (i == 31) {
                    pending_quote_ = true;
                    pos_ = event + 1;
                } else if (event[1] == '"') {
                    ++escaped_pairs;
                    pos_ = event + 2;
                } else {
                    in_quotes_ = false;
                    pos_ = event + 1;
                }
            } else {
                if (!in_quotes_) {
                    delimiter = *event;
                    at = event;
                    pos_ = event + 1;
                    return true;
                }
                pos_ = event + 1;
            }
        }
        return false;
    }

    inline bool previous_is_cr(const char* at) const {
        if (at > block_ && at <= block_ + 32) {
            const u32 i = u32(at - block_ - 1);
            return (crs_ >> i) & 1;
        }
        return at[-1] == '\r';
    }

private:
    inline void load_block() {
        block_ = pos_;
        const __m256i v = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(block_));
        quotes_ = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8('"')));
        commas_ = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(',')));
        lfs_ = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8('\n')));
        crs_ = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8('\r')));
        const size_t remaining = size_t(end_ - block_);
        if (remaining < 32) {
            const u32 valid = (u32(1) << remaining) - 1;
            quotes_ &= valid;
            commas_ &= valid;
            lfs_ &= valid;
            crs_ &= valid;
        }
    }

    const char* pos_;
    const char* end_;
    const char* block_ = nullptr;
    u32 quotes_ = 0, commas_ = 0, lfs_ = 0, crs_ = 0;
    bool in_quotes_ = false, pending_quote_ = false;
};

// Names, interned by their raw bytes between the quotes. This class is frozen
// from v172: 1 MB, 8-byte atomic slots, four-slot AVX2 probes, same key function.
class NameTable {
public:
    static constexpr u32 BITS = 17, SLOTS = 1 << BITS;

    static u64 key(const char* s, u32 len) {
        static constexpr char KEEP[32] = {-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1};
        __m128i head = _mm_loadu_si128(reinterpret_cast<const __m128i*>(s));
        head = _mm_and_si128(head, _mm_loadu_si128(reinterpret_cast<const __m128i*>(KEEP + 16 - std::min(len, 16u))));
        u64 h = (u64(_mm_cvtsi128_si64(head)) * 0x9E3779B97F4A7C15ull ^ len) + u64(_mm_extract_epi64(head, 1)) * 0xBF58476D1CE4E5B9ull;
        for (u32 i = 16; i < len; i += 8) {
            u64 w = load8(s + i);
            if (len - i < 8) w &= (u64(1) << 8 * (len - i)) - 1;
            h = (h ^ w) * 0x94D049BB133111EBull;
        }
        h = (h ^ h >> 31) * 0xD6E8FEB86659FD93ull;
        h ^= h >> 32;
        return h + (h == 0);
    }

    void prefetch(u64 k) const {
        _mm_prefetch(reinterpret_cast<const char*>(&slots_[k >> (64 - BITS)]), _MM_HINT_T0);
    }

    u32 id(u64 k) {
        for (u32 i = k >> (64 - BITS) & ~3u;; i = (i + 4) % SLOTS) {
            __m256i v = _mm256_load_si256(reinterpret_cast<const __m256i*>(&slots_[i]));
            u32 hit = _mm256_movemask_pd(_mm256_castsi256_pd(_mm256_cmpeq_epi64(v, _mm256_set1_epi64x(k))));
            u32 free = _mm256_movemask_pd(_mm256_castsi256_pd(_mm256_cmpeq_epi64(v, _mm256_setzero_si256())));
            if (hit) return i + __builtin_ctz(hit);
            for (; free; free &= free - 1) {
                u32 j = i + __builtin_ctz(free);
                u64 s = 0;
                if (slots_[j].compare_exchange_strong(s, k) || s == k) return j;
            }
        }
    }

    void populate(int t, int n) const { populate_share(slots_, SLOTS, t, n); }

private:
    std::atomic<u64>* slots_ = zeroed<std::atomic<u64>>(SLOTS);
};

struct alignas(64) Totals {
    u64 count = 0, id_sum = 0, name_bytes = 0, note_bytes = 0, distinct = 0;
    i64 amount_sum = 0, amount_min = INT64_MAX, amount_max = INT64_MIN;
};

struct NamesSeen {
    static constexpr u32 WORDS = NameTable::SLOTS / 64;
    std::atomic<u64>* bits = zeroed<std::atomic<u64>>(256 * WORDS);

    std::atomic<u64>& word(u32 c, u32 name) { return bits[c * WORDS + name / 64]; }
    void prefetch(u32 c, u32 name) { __builtin_prefetch(&word(c, name), 1); }
    bool mark(u32 c, u32 name) {
        const u64 bit = u64(1) << name % 64;
        return !(word(c, name).fetch_or(bit, std::memory_order_relaxed) & bit);
    }
};

void parse_records(const char* begin, const char* end,
                   const u8* country_of, size_t regions, NameTable& names,
                   NamesSeen& seen, Totals* totals) {
    constexpr u64 LAG = 16;
    struct Pending {
        u64 name;
        u32 region, id, name_bytes, note_bytes;
        i64 cents;
    } pending[LAG];
    struct Mark { u32 country, id; } marks[LAG];
    RecordScanner scan(begin, end);
    u64 n = 0, last = UINT64_MAX;

    auto next = [&](char expected, const char*& field_end, u32& escaped) __attribute__((always_inline)) {
        char delimiter;
        if (!scan.next(delimiter, field_end, escaped) || delimiter != expected) _exit(2);
    };

    auto advance = [&]() __attribute__((always_inline)) {
        const u32 k = n % LAG;
        if (n >= 2 * LAG && n - 2 * LAG < last)
            totals[marks[k].country].distinct += seen.mark(marks[k].country, marks[k].id);
        if (n >= LAG && n - LAG < last) {
            const Pending& old = pending[k];
            const u32 c = old.region < regions ? country_of[old.region] : 0;
            Totals& t = totals[c];
            ++t.count;
            t.id_sum += old.id;
            t.amount_sum += old.cents;
            t.amount_min = std::min(t.amount_min, old.cents);
            t.amount_max = std::max(t.amount_max, old.cents);
            t.name_bytes += old.name_bytes;
            t.note_bytes += old.note_bytes;
            marks[k] = {c, names.id(old.name)};
            seen.prefetch(c, marks[k].id);
        }
    };

    while (scan.position() < end) {
        const char* p = scan.position();
        _mm_prefetch(p + 2048, _MM_HINT_T0);
        if (u8(*p) == 0xEF) scan.skip(3);

        const char* id_start = scan.position();
        const char* id_end;
        u32 escaped;
        next(',', id_end, escaped);
        const u32 id = u32(digits(id_start, id_end));

        const char* name_start = scan.position();
        const char* name_delim;
        u32 name_escaped;
        next(',', name_delim, name_escaped);
        const bool name_quoted = *name_start == '"';
        const char* name_data = name_start + name_quoted;
        const char* name_end = name_delim - name_quoted;
        const u32 name_raw = u32(name_end - name_data);
        const u32 name_bytes = name_raw - name_escaped;

        const char* note_start = scan.position();
        const char* note_delim;
        u32 note_escaped;
        next(',', note_delim, note_escaped);
        const bool note_quoted = *note_start == '"';
        const char* note_data = note_start + note_quoted;
        const char* note_end = note_delim - note_quoted;
        const u32 note_raw = u32(note_end - note_data);
        const u32 note_bytes = note_raw - note_escaped;

        const char* region_start = scan.position();
        const char* region_end;
        next(',', region_end, escaped);
        const u64 region = digits(region_start, region_end);

        const char* amount_start = scan.position();
        const char* eol;
        next('\n', eol, escaped);
        const char* amount_end = eol - scan.previous_is_cr(eol);
        const bool negative = *amount_start == '-';
        i64 cents = money(amount_start + negative, amount_end);
        if (negative) cents = -cents;

        const u32 r = region < regions ? u32(region) : u32(regions);
        _mm_prefetch(reinterpret_cast<const char*>(country_of + r), _MM_HINT_T0);
        advance();
        const u64 key = NameTable::key(name_data, name_raw);
        pending[n++ % LAG] = {key, r, id, name_bytes, note_bytes, cents};
        names.prefetch(key);
    }

    for (last = n; n < last + 2 * LAG; ++n)
        advance();
}

u32 code(const char* p) { return (p[0] - 'A') << 10 | (p[1] - 'A') << 5 | (p[2] - 'A'); }

std::pair<u64, u32> region_line(const char*& p) {
    const char* comma = comma_after(p);
    std::pair<u64, u32> line{digits(p, comma), code(comma + 1)};
    for (p = comma + 4; *p++ != '\n';) {}
    return line;
}

struct Phase {
    std::atomic<int> claimed{0}, done{0};
    const int pieces;
    template <class F>
    void run(F&& f) {
        for (int k; (k = claimed++) < pieces;)
            if (f(k), ++done == pieces) done.notify_all();
        for (int d; (d = done) < pieces;) done.wait(d);
    }
};

template <size_t N>
std::vector<size_t> split(const char* data, size_t from, size_t size,
                          int n, const char (&delim)[N]) {
    std::vector<size_t> cut(n + 1, size);
    cut[0] = from;
    for (int t = 1; t < n; ++t) {
        const size_t c = std::max(cut[t - 1], from + (size - from) * t / n);
        auto* hit = static_cast<const char*>(memmem(data + c, size - c, delim, N - 1));
        cut[t] = hit ? size_t(hit - data) + N - 1 : size;
    }
    return cut;
}

void put_money(std::string& out, i64 cents) {
    const u64 a = cents < 0 ? -u64(cents) : u64(cents);
    out += cents < 0 ? " -" : " ";
    out += std::to_string(a / 100);
    out += {'.', char('0' + a % 100 / 10), char('0' + a % 10)};
}

int main(int argc, char** argv) {
    prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0);
    const int nthreads = std::clamp<int>(std::thread::hardware_concurrency(), 1, 12);
    size_t rsize, size;
    const std::string dd = argc > 1 ? argv[1] : "/data";
    const std::string od = argc > 2 ? argv[2] : "/out";
    const char* regions = map_file((dd + "/regions.csv").c_str(), rsize);
    const char* records = map_file((dd + "/records.csv").c_str(), size);

    const char* last_region = static_cast<const char*>(memrchr(regions, '\n', rsize - 1)) + 1;
    const size_t nregions = atoll(last_region) + 1;
    u8* country_of = zeroed<u8>(nregions);
    const size_t rstart = size_t(strchr(regions, '\n') - regions) + 1;
    std::vector<size_t> rcut = split(regions, rstart, rsize, nthreads, "\n");
    std::vector<std::atomic<u8>> present(1 << 15);
    std::array<u8, 1 << 15> index_of{};
    std::vector<u32> codes;
    std::once_flag numbered;
    auto number_countries = [&] {
        for (u32 k = 0; k < present.size(); ++k)
            if (present[k]) codes.push_back(k), index_of[k] = codes.size();
        if (codes.size() > 255) { fputs("too many countries\n", stderr); _exit(1); }
    };

    const size_t start = size ? size_t(static_cast<const char*>(memchr(records, '\n', size)) - records) + 1 : 0;
    const int pieces = 8 * nthreads;
    std::vector<size_t> cut = split(records, start, size, pieces, "\r\n");
    std::vector<std::array<Totals, 256>> totals(nthreads);
    NameTable names;
    NamesSeen seen;
    Phase flag{.pieces = nthreads}, fill{.pieces = nthreads}, parse{.pieces = pieces};
    std::vector<std::thread> pool;
    for (int t = 0; t < nthreads; ++t)
        pool.emplace_back([&, t] {
            flag.run([&](int k) {
                populate_share(country_of, nregions, k, nthreads);
                names.populate(k, nthreads);
                for (const char* p = regions + rcut[k]; p < regions + rcut[k + 1];)
                    present[region_line(p).second].store(1, std::memory_order_relaxed);
            });
            std::call_once(numbered, number_countries);
            fill.run([&](int k) {
                populate_share(seen.bits, (codes.size() + 1) * NamesSeen::WORDS, k, nthreads);
                for (const char* p = regions + rcut[k]; p < regions + rcut[k + 1];) {
                    auto [id, c] = region_line(p);
                    country_of[id] = index_of[c];
                }
            });
            parse.run([&](int k) {
                const uintptr_t from = uintptr_t(records + cut[k]) & ~uintptr_t(4095);
                madvise(reinterpret_cast<void*>(from),
                        uintptr_t(records + cut[k + 1]) - from, MADV_POPULATE_READ);
                parse_records(records + cut[k], records + cut[k + 1],
                              country_of, nregions, names, seen, totals[t].data());
                const uintptr_t to = uintptr_t(records + cut[k + 1]) & ~uintptr_t(4095);
                if (to > from)
                    madvise(reinterpret_cast<void*>(from), to - from, MADV_DONTNEED);
            });
        });
    for (auto& th : pool) th.join();

    std::string out;
    for (u32 i = 1; i <= codes.size() + 1; ++i) {
        const u32 c = i % (codes.size() + 1);
        Totals sum;
        for (const auto& per_thread : totals) {
            const Totals& t = per_thread[c];
            sum.count += t.count;
            sum.id_sum += t.id_sum;
            sum.name_bytes += t.name_bytes;
            sum.note_bytes += t.note_bytes;
            sum.distinct += t.distinct;
            sum.amount_sum += t.amount_sum;
            sum.amount_min = std::min(sum.amount_min, t.amount_min);
            sum.amount_max = std::max(sum.amount_max, t.amount_max);
        }
        if (sum.count == 0) continue;
        if (c == 0) out += "_UNMATCHED";
        else {
            const u32 k = codes[c - 1];
            out += {char('A' + (k >> 10)), char('A' + (k >> 5 & 31)), char('A' + (k & 31))};
        }
        for (u64 v : {sum.count, sum.id_sum}) out += ' ' + std::to_string(v);
        for (i64 v : {sum.amount_sum, sum.amount_min, sum.amount_max}) put_money(out, v);
        for (u64 v : {sum.distinct, sum.name_bytes, sum.note_bytes}) out += ' ' + std::to_string(v);
        out += '\n';
    }
    FILE* f = fopen((od + "/aggregates.txt").c_str(), "wb");
    if (!f) { perror("aggregates.txt"); return 1; }
    fwrite(out.data(), 1, out.size(), f);
    fclose(f);
    _exit(0);
}
