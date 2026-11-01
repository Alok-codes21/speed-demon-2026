// Reused Track C code and design fragments from:
// Cache Me / manusingh090, speed-demon-2026-entry
// https://github.com/manusingh090/speed-demon-2026-entry
// Copyright (c) 2026 Hackathon Raptors CIC
// Copyright (c) 2026 the contributors of that repository
// The two-pass dictionary, exact-span collision checks, and related changes
// in this candidate are original modifications.
//
// MIT License
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to
// deal in the Software without restriction, including without limitation the
// rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
// sell copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
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
#include <limits>
#include <mutex>
#include <string>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <thread>
#include <unistd.h>
#include <utility>
#include <vector>

using u8 = uint8_t;
using u32 = uint32_t;
using u64 = uint64_t;
using i64 = int64_t;

template <class T>
T* zeroed(size_t n) {
    void* p = mmap(nullptr, n * sizeof(T), PROT_READ | PROT_WRITE,
                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) {
        perror("mmap");
        _exit(1);
    }
    return static_cast<T*>(p);
}

template <class T>
void populate_share(T* p, size_t n, int t, int nthreads) {
    const size_t pages = (n * sizeof(T) + 4095) / 4096;
    const size_t from = pages * t / nthreads;
    const size_t to = pages * (t + 1) / nthreads;
    if (to > from)
        madvise(reinterpret_cast<char*>(p) + from * 4096,
                (to - from) * 4096, MADV_POPULATE_WRITE);
}

const char* map_file(const char* path, size_t& size) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        perror(path);
        _exit(1);
    }

    off_t end = lseek(fd, 0, SEEK_END);
    if (end < 0) {
        perror("lseek");
        _exit(1);
    }

    size = static_cast<size_t>(end);
    auto* p = static_cast<char*>(
        mmap(nullptr, size + 4096, PROT_READ,
             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0));

    if (p == MAP_FAILED) {
        perror("mmap anonymous");
        _exit(1);
    }

    if (size && mmap(p, size, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0) == MAP_FAILED) {
        perror("mmap file");
        _exit(1);
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

inline u32 match32(const char* p, char c) {
    __m256i v = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(p));
    return static_cast<u32>(
        _mm256_movemask_epi8(
            _mm256_cmpeq_epi8(v, _mm256_set1_epi8(c))));
}

inline u64 fold(u64 v, size_t n) {
    v &= 0x0F0F0F0F0F0F0F0Full & (~u64(0) << (8 * (8 - n)));
    v = (v * 10 + (v >> 8)) & 0x00FF00FF00FF00FFull;
    v = (v * 100 + (v >> 16)) & 0x0000FFFF0000FFFFull;
    return (v * 10000 + (v >> 32)) & 0xFFFFFFFFull;
}

inline u64 digits(const char* s, const char* e) {
    return fold(load8(e - 8), e - s);
}

inline u64 money(const char* s, const char* e) {
    u64 w = load8(e - 8);
    return fold((w & 0xFFFFFFFFFFull) << 8 |
                (w & 0xFFFF000000000000ull), e - s - 1);
}

[[gnu::always_inline]] inline const char* find(const char* p, char c) {
    for (;; p += 64) {
        if (u64 m = u64(match32(p, c)) |
                    (u64(match32(p + 32, c)) << 32))
            return p + __builtin_ctzll(m);
    }
}

inline const char* comma_after(const char* p) {
    u64 x = load8(p) ^ 0x2C2C2C2C2C2C2C2Cull;
    return p + (__builtin_ctzll(
        (x - 0x0101010101010101ull) &
        ~x & 0x8080808080808080ull) / 8);
}

struct Text {
    const char* at;
    u32 raw, length;
};

[[gnu::always_inline]] inline Text text(const char*& p) {
    const char* s = p;

    if (*s != '"') {
        p = find(s, ',') + 1;
        return {s, u32(p - 1 - s), u32(p - 1 - s)};
    }

    u32 pairs = 0;
    const char* q = s + 1;
    u64 m = u64(match32(q, '"')) |
            (u64(match32(q + 32, '"')) << 32);

    for (;;) {
        u32 i = m ? __builtin_ctzll(m) : 64;

        if (i >= 63) {
            q += i;
            m = u64(match32(q, '"')) |
                (u64(match32(q + 32, '"')) << 32);
            continue;
        }

        if (!(m >> (i + 1) & 1)) {
            q += i;
            break;
        }

        ++pairs;
        m &= m - 1;
        m &= m - 1;
    }

    p = q + 2;
    return {s + 1, u32(q - s - 1), u32(q - s - 1 - pairs)};
}

inline u64 name_hash(const char* s, u32 len) {
    static constexpr char KEEP[32] = {
        -1, -1, -1, -1, -1, -1, -1, -1,
        -1, -1, -1, -1, -1, -1, -1, -1
    };

    __m128i head = _mm_loadu_si128(reinterpret_cast<const __m128i*>(s));
    head = _mm_and_si128(
        head,
        _mm_loadu_si128(
            reinterpret_cast<const __m128i*>(
                KEEP + 16 - std::min(len, 16u))));

    u64 h =
        (u64(_mm_cvtsi128_si64(head)) * 0x9E3779B97F4A7C15ull ^ len) +
        u64(_mm_extract_epi64(head, 1)) * 0xBF58476D1CE4E5B9ull;

    for (u32 i = 16; i < len; i += 8) {
        u64 w = load8(s + i);
        if (len - i < 8)
            w &= (u64(1) << (8 * (len - i))) - 1;
        h = (h ^ w) * 0x94D049BB133111EBull;
    }

    h = (h ^ (h >> 31)) * 0xD6E8FEB86659FD93ull;
    h ^= h >> 32;
    return h;
}

struct NameRef {
    u32 offset, raw;
};

class NameSet {
public:
    static constexpr u32 BITS = 18;
    static constexpr u32 SLOTS = 1u << BITS;
    static constexpr u32 MASK = SLOTS - 1;
    static constexpr u64 BUSY = 1;
    static constexpr u64 READY = u64(1) << 63;

    explicit NameSet(const char* base) : base_(base) {
        state_ = zeroed<std::atomic<u64>>(SLOTS);
        refs_ = zeroed<NameRef>(SLOTS);
    }

    void populate(int t, int n) {
        populate_share(state_, SLOTS, t, n);
        populate_share(refs_, SLOTS, t, n);
    }

    inline void prefetch(u64 h) const {
        __builtin_prefetch(&state_[u32(h) & MASK], 0, 3);
    }

    inline void insert(u64 h, const char* s, u32 len) {
        const u64 tag = READY | (h & (READY - 1));
        u32 i = u32(h) & MASK;

        for (;;) {
            u64 v = state_[i].load(std::memory_order_acquire);

            if (v == 0) {
                u64 expected = 0;
                if (state_[i].compare_exchange_strong(
                        expected, BUSY,
                        std::memory_order_acq_rel,
                        std::memory_order_acquire)) {
                    refs_[i] = {u32(s - base_), len};
                    state_[i].store(tag, std::memory_order_release);
                    return;
                }
                v = expected;
            }

            if (v == BUSY) {
                do {
                    _mm_pause();
                    v = state_[i].load(std::memory_order_acquire);
                } while (v == BUSY);
            }

            if (v == tag) {
                const NameRef& r = refs_[i];
                if (r.raw == len && !memcmp(base_ + r.offset, s, len))
                    return;
            }

            i = (i + 1) & MASK;
        }
    }

    void make_dictionary(std::vector<NameRef>& out) const {
        out.clear();
        out.reserve(100000);

        for (u32 i = 0; i < SLOTS; ++i) {
            u64 v = state_[i].load(std::memory_order_acquire);
            if (v > BUSY)
                out.push_back(refs_[i]);
        }
    }

    void release() {
        munmap(state_, SLOTS * sizeof(std::atomic<u64>));
        munmap(refs_, SLOTS * sizeof(NameRef));
        state_ = nullptr;
        refs_ = nullptr;
    }

private:
    const char* base_;
    std::atomic<u64>* state_ = nullptr;
    NameRef* refs_ = nullptr;
};

class NameDictionary {
public:
    void build(const char* base, NameSet& set) {
        set.make_dictionary(refs_);
        set.release();

        std::sort(refs_.begin(), refs_.end(),
                  [&](const NameRef& a, const NameRef& b) {
            const u32 n = std::min(a.raw, b.raw);
            const int c = n ? memcmp(base + a.offset, base + b.offset, n) : 0;
            return c ? c < 0 : a.raw < b.raw;
        });

        size_t total = 0;
        for (const NameRef& r : refs_)
            total += r.raw;

        if (total > UINT32_MAX) {
            fputs("name dictionary exceeds 4 GB\n", stderr);
            _exit(2);
        }

        pool_.reserve(total);
        for (NameRef& r : refs_) {
            const u32 offset = u32(pool_.size());
            pool_.insert(pool_.end(),
                         base + r.offset,
                         base + r.offset + r.raw);
            r.offset = offset;
        }

        for (u32 i = 0; i <= 65536; ++i)
            bounds_[i] = 0;

        for (const NameRef& r : refs_)
            ++bounds_[prefix(pool_.data() + r.offset, r.raw) + 1];

        for (u32 i = 1; i <= 65536; ++i)
            bounds_[i] += bounds_[i - 1];
    }

    static inline u32 prefix(const char* s, u32 n) {
        const u32 a = n ? static_cast<u8>(s[0]) : 0;
        const u32 b = n > 1 ? static_cast<u8>(s[1]) : 0;
        return (a << 8) | b;
    }

    inline void prefetch(const char* s, u32 n) const {
        const u32 p = prefix(s, n);
        const u32 lo = bounds_[p], hi = bounds_[p + 1];

        if (lo < hi) {
            const NameRef& r = refs_[lo + ((hi - lo) >> 1)];
            __builtin_prefetch(&r, 0, 3);
            __builtin_prefetch(pool_.data() + r.offset, 0, 3);
        }
    }

    inline u32 id(const char* s, u32 n) const {
        u32 lo = bounds_[prefix(s, n)];
        u32 hi = bounds_[prefix(s, n) + 1];

        while (lo < hi) {
            const u32 mid = lo + ((hi - lo) >> 1);
            const NameRef& r = refs_[mid];
            const u32 m = std::min(n, r.raw);
            const int c = m ? memcmp(s, pool_.data() + r.offset, m) : 0;

            if (c == 0 && n == r.raw)
                return mid;

            if (c < 0 || (c == 0 && n < r.raw))
                hi = mid;
            else
                lo = mid + 1;
        }

        fputs("name missing from extracted dictionary\n", stderr);
        _exit(2);
    }

    size_t size() const {
        return refs_.size();
    }

private:
    std::vector<char> pool_;
    std::vector<NameRef> refs_;
    std::array<u32, 65537> bounds_{};
};

void extract_names(const char* p, const char* end, NameSet& names) {
    constexpr u32 LAG = 16;

    struct Pending {
        u64 hash;
        const char* at;
        u32 raw;
    } pending[LAG];

    u64 n = 0;

    while (p < end) {
        _mm_prefetch(p + 2048, _MM_HINT_T0);
        if (u8(*p) == 0xEF)
            p += 3;

        const char* id_comma = comma_after(p);
        p = id_comma + 1;

        Text name = text(p);
        text(p);

        const char* region_comma = comma_after(p);
        const char* eol = find(region_comma + 1, '\n');

        const u64 h = name_hash(name.at, name.raw);

        // Retire the old ring entry before overwriting its slot.
        if (n >= LAG) {
            const Pending& old = pending[(n - LAG) % LAG];
            names.insert(old.hash, old.at, old.raw);
        }

        pending[n % LAG] = {h, name.at, name.raw};
        names.prefetch(h);
        ++n;
        p = eol + 1;
    }

    const u64 first = n > LAG ? n - LAG : 0;
    for (u64 i = first; i < n; ++i) {
        const Pending& old = pending[i % LAG];
        names.insert(old.hash, old.at, old.raw);
    }
}

struct alignas(64) Totals {
    u64 count = 0, id_sum = 0, name_bytes = 0, note_bytes = 0, distinct = 0;
    i64 amount_sum = 0, amount_min = INT64_MAX, amount_max = INT64_MIN;
};

struct NamesSeen {
    size_t words;
    std::atomic<u64>* bits;

    explicit NamesSeen(size_t n)
        : words((n + 63) / 64),
          bits(zeroed<std::atomic<u64>>(256 * words)) {}

    std::atomic<u64>& word(u32 c, u32 id) {
        return bits[size_t(c) * words + id / 64];
    }

    void prefetch(u32 c, u32 id) {
        __builtin_prefetch(&word(c, id), 1, 3);
    }

    bool mark(u32 c, u32 id) {
        const u64 bit = u64(1) << (id & 63);
        return !(word(c, id).fetch_or(bit, std::memory_order_relaxed) & bit);
    }
};

void parse_records(const char* p, const char* end, const char* file_end,
                   const u8* country_of, size_t regions,
                   const NameDictionary& names, NamesSeen& seen,
                   Totals* totals) {
    constexpr u64 LAG = 16;

    struct Pending {
        const char* name;
        u32 name_raw, region, id, name_bytes, note_bytes;
        i64 cents;
    } pending[LAG];

    struct Mark {
        u32 country, id;
    } marks[LAG];

    u64 n = 0, last = UINT64_MAX;

    auto advance = [&]() __attribute__((always_inline)) {
        const u32 k = n % LAG;

        if (n >= 2 * LAG && n - 2 * LAG < last)
            totals[marks[k].country].distinct +=
                seen.mark(marks[k].country, marks[k].id);

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

            marks[k] = {c, names.id(old.name, old.name_raw)};
            seen.prefetch(c, marks[k].id);
        }
    };

    while (p < end) {
        _mm_prefetch(p + 2048, _MM_HINT_T0);
        if (u8(*p) == 0xEF)
            p += 3;

        const char* f1 = comma_after(p);
        const u64 id = digits(p, f1);
        p = f1 + 1;

        Text name = text(p), note = text(p);

        const u32 lf = match32(p, '\n');
        const u32 comma = match32(p, ',');
        const char* f4 = p + __builtin_ctz(comma);
        const u64 region = digits(p, f4);

        const char* amount = f4 + 1;
        const char* eol = lf ? p + __builtin_ctz(lf) : file_end;
        const char* amount_end = eol - (eol[-1] == '\r');

        const bool negative = *amount == '-';
        i64 cents = money(amount + negative, amount_end);
        if (negative)
            cents = -cents;

        const u32 r = region < regions ? u32(region) : u32(regions);
        __builtin_prefetch(country_of + r, 0, 3);
        names.prefetch(name.at, name.raw);

        advance();

        pending[n++ % LAG] = {
            name.at, name.raw, r, u32(id),
            name.length, note.length, cents
        };

        p = eol + 1;
    }

    for (last = n; n < last + 2 * LAG; ++n)
        advance();
}

inline u32 code(const char* p) {
    return (u32(p[0] - 'A') << 10) |
           (u32(p[1] - 'A') << 5) |
           u32(p[2] - 'A');
}

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
            if (f(k), ++done == pieces)
                done.notify_all();

        for (int d; (d = done.load(std::memory_order_acquire)) < pieces;)
            done.wait(d, std::memory_order_relaxed);
    }
};

template <size_t N>
std::vector<size_t> split(const char* data, size_t from, size_t size,
                          int n, const char (&delim)[N]) {
    std::vector<size_t> cut(n + 1, size);
    cut[0] = from;

    for (int t = 1; t < n; ++t) {
        const size_t c =
            std::max(cut[t - 1], from + (size - from) * t / n);
        auto* hit = static_cast<const char*>(
            memmem(data + c, size - c, delim, N - 1));
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
    constexpr int nthreads = 12;

    size_t rsize, size;
    const std::string dd = argc > 1 ? argv[1] : "/data";
    const std::string od = argc > 2 ? argv[2] : "/out";

    const char* regions = map_file((dd + "/regions.csv").c_str(), rsize);
    const char* records = map_file((dd + "/records.csv").c_str(), size);

    if (size > UINT32_MAX)
        _exit(2);

    const char* last_region =
        static_cast<const char*>(memrchr(regions, '\n', rsize - 1)) + 1;
    const size_t nregions = atoll(last_region) + 1;

    u8* country_of = zeroed<u8>(nregions);
    const size_t rstart =
        size_t(strchr(regions, '\n') - regions) + 1;
    std::vector<size_t> rcut =
        split(regions, rstart, rsize, nthreads, "\n");

    std::vector<std::atomic<u8>> present(1 << 15);
    std::array<u8, 1 << 15> index_of{};
    std::vector<u32> codes;
    std::once_flag numbered;

    auto number_countries = [&] {
        for (u32 k = 0; k < present.size(); ++k) {
            if (present[k].load(std::memory_order_relaxed)) {
                codes.push_back(k);
                index_of[k] = u8(codes.size());
            }
        }

        if (codes.size() > 255) {
            fputs("too many countries\n", stderr);
            _exit(1);
        }
    };

    const size_t start =
        size ? size_t(static_cast<const char*>(
            memchr(records, '\n', size)) - records) + 1 : 0;

    const int pieces = 8 * nthreads;
    std::vector<size_t> cut =
        split(records, start, size, pieces, "\r\n");

    NameSet discovered(records);
    NameDictionary dictionary;
    NamesSeen* seen = nullptr;

    std::vector<std::array<Totals, 256>> totals(nthreads);
    std::once_flag dictionary_ready;

    Phase init_names{.pieces = nthreads};
    Phase extract{.pieces = pieces};
    Phase flag{.pieces = nthreads};
    Phase fill{.pieces = nthreads};
    Phase parse{.pieces = pieces};

    std::vector<std::thread> pool;
    pool.reserve(nthreads);

    for (int t = 0; t < nthreads; ++t) {
        pool.emplace_back([&, t] {
            init_names.run([&](int k) {
                discovered.populate(k, nthreads);
            });

            extract.run([&](int k) {
                const uintptr_t from =
                    uintptr_t(records + cut[k]) & ~uintptr_t(4095);
                const uintptr_t to =
                    (uintptr_t(records + cut[k + 1]) + 4095) &
                    ~uintptr_t(4095);

                if (to > from)
                    madvise(reinterpret_cast<void*>(from), to - from,
                            MADV_POPULATE_READ);

                extract_names(records + cut[k],
                              records + cut[k + 1], discovered);
            });

            std::call_once(dictionary_ready, [&] {
                dictionary.build(records, discovered);
                seen = new NamesSeen(dictionary.size());
            });

            flag.run([&](int k) {
                populate_share(country_of, nregions, k, nthreads);

                for (const char* p = regions + rcut[k];
                     p < regions + rcut[k + 1];)
                    present[region_line(p).second].store(
                        1, std::memory_order_relaxed);
            });

            std::call_once(numbered, number_countries);

            fill.run([&](int k) {
                populate_share(seen->bits, 256 * seen->words,
                               k, nthreads);

                for (const char* p = regions + rcut[k];
                     p < regions + rcut[k + 1];) {
                    auto [id, c] = region_line(p);
                    country_of[id] = index_of[c];
                }
            });

            parse.run([&](int k) {
                const uintptr_t from =
                    uintptr_t(records + cut[k]) & ~uintptr_t(4095);
                const uintptr_t to =
                    uintptr_t(records + cut[k + 1]) & ~uintptr_t(4095);

                if (to > from)
                    madvise(reinterpret_cast<void*>(from), to - from,
                            MADV_POPULATE_READ);

                parse_records(records + cut[k], records + cut[k + 1],
                              records + size, country_of, nregions,
                              dictionary, *seen, totals[t].data());

                if (to > from)
                    madvise(reinterpret_cast<void*>(from), to - from,
                            MADV_DONTNEED);
            });
        });
    }

    for (auto& th : pool)
        th.join();

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

        if (sum.count == 0)
            continue;

        if (c == 0) {
            out += "_UNMATCHED";
        } else {
            const u32 k = codes[c - 1];
            out += {
                char('A' + (k >> 10)),
                char('A' + (k >> 5 & 31)),
                char('A' + (k & 31))
            };
        }

        for (u64 v : {sum.count, sum.id_sum})
            out += ' ' + std::to_string(v);

        for (i64 v : {sum.amount_sum, sum.amount_min, sum.amount_max})
            put_money(out, v);

        for (u64 v : {sum.distinct, sum.name_bytes, sum.note_bytes})
            out += ' ' + std::to_string(v);

        out += '\n';
    }

    FILE* f = fopen((od + "/aggregates.txt").c_str(), "wb");
    if (!f) {
        perror("aggregates.txt");
        return 1;
    }

    fwrite(out.data(), 1, out.size(), f);
    fclose(f);
    _exit(0);
}
