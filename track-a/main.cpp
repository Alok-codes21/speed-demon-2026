/*
MIT License

Copyright (c) 2026 Hackathon Raptors CIC
Copyright (c) 2026 the contributors of this repository

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

*/
// ---------------------------------------------------------------------------
// Prior Track A engine credit retained from HighVelocity's public repository
//   https://github.com/insightlabs38-pixel/sd-sm
// at commit 8b6f45035580b379f8e27af9cb537c4068896c09 (identical to their
// board-best commit 12673d75a84ce13de0c21a687a605c5c86327d96 for this file),
// This trial alternative replaces that engine with the separately credited
// Cache Me implementation below; this notice preserves prior attribution.
// Their code is MIT licensed (Hackathon Raptors CIC and contributors); the
// full license text is in THIRD_PARTY_LICENSES.md in this directory and the
// copyright/permission notice below is retained as required:
//
// Copyright (c) 2026 Hackathon Raptors CIC
// Copyright (c) 2026 the contributors of this repository (HighVelocity)
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
// the above copyright notice and this permission notice shall be included
// in all copies or substantial portions of the Software.
// ---------------------------------------------------------------------------
// Trial alternative engine: Cache Me, MIT licensed, source
// https://github.com/manusingh090/speed-demon-2026-entry
// commit 7027ab49162605eea30311a351ebeeef3647efdc, track-a/main.cpp.
// Copyright (c) 2026 Hackathon Raptors CIC and repository contributors.
// Local changes: replace the three full-array radix passes with one endpoint
// grouping pass and in-place percentile selection; record layout is unchanged.
// The complete Cache Me MIT license is retained at the top of this file.
#include <algorithm>
#include <atomic>
#include <barrier>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fcntl.h>
#include <immintrin.h>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <thread>
#include <unistd.h>
#include <utility>
#include <vector>

using u32 = uint32_t;
using u64 = uint64_t;
constexpr int HOURS = 72; // 2026-10-30T18:00Z to 2026-11-02T17:59:59Z
// A record packs the latency above a 12-bit key: the 5xx flag and the
// endpoint id plus one, so a place never written (zero) is no record.
// Latencies of 2^20 - 1 us or more (about 1% of lines) take two places: a
// record whose latency field is all ones (ESCAPE), then the raw value.
constexpr u32 KEY_BITS = 12, ERR = 1u << 11, ID_MASK = ERR - 1,
              PACKED_MAX = (1u << 20) - 1, ESCAPE = PACKED_MAX << KEY_BITS;

// The number of decimal digits in x, from its bit length.
inline u32 width(u32 x) {
  static constexpr u32 p10[] = {1,         10,        100,     1000,
                                10000,     100000,    1000000, 10000000,
                                100000000, 1000000000};
  u32 t = (32 - __builtin_clz(x | 1)) * 1233 >> 12;
  return t + ((x | 1) >= p10[t]);
}

// Writes x in decimal ending at end, whose width is already known, two digits
// at a time from a table of the 100 pairs.
inline void put(char *end, u32 x) {
  static constexpr char pairs[] =
      "000102030405060708091011121314151617181920212223242526272829303132333435"
      "363738394041424344454647484950515253545556575859606162636465666768697071"
      "72737475767778798081828384858687888990919293949596979899";
  for (; x >= 100; x /= 100)
    memcpy(end -= 2, &pairs[x % 100 * 2], 2);
  if (x >= 10)
    memcpy(end - 2, &pairs[x * 2], 2);
  else
    end[-1] = '0' + x;
}

inline __m256i load(const char *p) {
  return _mm256_loadu_si256((const __m256i *)p);
}

// Bit i is set when byte i of the 64 in a and b equals the same byte of x, y.
inline u64 eq64(__m256i a, __m256i b, __m256i x, __m256i y) {
  return u32(_mm256_movemask_epi8(_mm256_cmpeq_epi8(a, x))) |
         u64(u32(_mm256_movemask_epi8(_mm256_cmpeq_epi8(b, y)))) << 32;
}

// Endpoint names to dense ids, one table for all threads. A slot of the
// open-addressing table (16 KB, so it stays in the core's first cache) holds
// a name's id plus one, found by a CRC of the name's first and last eight
// bytes. Names sit 128 bytes apart, each followed by a space, as a name in a
// line is: the name and its space match a stored one exactly when the names
// are equal, so a name of up to 63 bytes is confirmed by the same two 32-byte
// loads that found its end. A new name is claimed with one compare-and-swap
// (the slot holds BUSY until the name is written), so no thread reads a
// half-written name.
struct Names {
  static constexpr u32 SLOTS = 1 << 13, MAX = ID_MASK, STRIDE = 128,
                       BUSY = 0xFFFF;
  std::atomic<uint16_t> slot[SLOTS];
  std::atomic<u32> ids{0};
  uint8_t length[MAX];
  alignas(64) char text[(MAX + 1) * STRIDE];

  std::string_view name(u32 id) const {
    return {text + id * STRIDE, length[id]};
  }

  // The id of the name of length n at s, whose first 64 bytes are v0, v1.
  u32 find(const char *s, u32 n, __m256i v0, __m256i v1) {
    u64 a, b;
    memcpy(&a, s, 8), memcpy(&b, s + n - 8, 8);
    if (n < 8)
      a = _bzhi_u64(a, 8 * n), b = 0;
    for (u32 h = _mm_crc32_u64(_mm_crc32_u64(n, a), b);; ++h) {
      u32 v = slot[h % SLOTS].load(std::memory_order_acquire);
      if (v - 1 >= MAX) [[unlikely]]
        return insert(h, s, n);
      const char *e = text + (v - 1) * STRIDE;
      u64 eq = eq64(v0, v1, load(e), load(e + 32));
      if (n < 64 ? (~eq & _bzhi_u64(~u64(0), n + 1)) == 0
                 : ~eq == 0 && (~eq64(load(s + 64), load(s + 96), load(e + 64),
                                      load(e + 96)) &
                                _bzhi_u64(~u64(0), n - 63)) == 0)
        return v - 1;
    }
  }

  [[gnu::noinline]] u32 insert(u32 h, const char *s, u32 n) {
    for (;;) {
      std::atomic<uint16_t> &at = slot[h % SLOTS];
      uint16_t v = at.load(std::memory_order_acquire);
      if (v == BUSY)
        continue;
      if (v == 0) {
        if (!at.compare_exchange_strong(v, BUSY))
          continue;
        u32 id = ids++;
        if (id >= MAX)
          _exit(3);
        memcpy(text + id * STRIDE, s, n);
        text[id * STRIDE + n] = ' ', length[id] = n;
        at.store(id + 1, std::memory_order_release);
        return id;
      }
      if (length[v - 1] == n && !memcmp(text + (v - 1) * STRIDE, s, n))
        return v - 1;
      ++h;
    }
  }
};
static Names endpoints;

// Endpoint ids in unsigned byte order of their names, and each id's place in
// it (indexed by id plus one, as in a record). A ranking is kept for as long
// as no name has been added since it was made.
struct Ranking {
  u32 ids;
  std::vector<u32> by_rank;
  std::vector<uint16_t> rank_of = std::vector<uint16_t>(ID_MASK + 1);

  Ranking() : ids(endpoints.ids) {
    std::vector<std::pair<std::string_view, u32>> by_name;
    for (u32 i = 0; i < Names::SLOTS; ++i)
      if (u32 v = endpoints.slot[i]; v && v != Names::BUSY)
        by_name.push_back({endpoints.name(v - 1), v - 1});
    std::sort(by_name.begin(), by_name.end());
    for (auto [name, id] : by_name)
      rank_of[id + 1] = by_rank.size(), by_rank.push_back(id);
    if (by_rank.size() != ids) // a name was being added: not kept
      ids = ~0u;
  }
};

// Hour h's records sit together from records + h * per_hour, filled by every
// thread in 4 KB blocks claimed with one atomic add. A thread filing into an
// hour counts itself in active[h] while it does; an hour is closed by setting
// closed[h] first and then finding active[h] zero, so a thread that enters
// a closed hour, or an hour closed while a thread was in it, is always seen
// (by the thread or by the closer) and the pass is redone.
constexpr u32 BLOCK = 1024;
static u32 *records;
static size_t per_hour;
static std::atomic<u32> filled[HOURS];
static std::atomic<int> active[HOURS];
static std::atomic<bool> closed[HOURS], late;

struct Parser {
  u32 *cur[HOURS] = {}, *stop[HOURS] = {};

  // Lines arrive in about clock order, so the hour index is worked out only
  // when the line's "MM-DDTHH" bytes differ from the line before's, and the
  // hour's place in its records stays in registers until then.
  //
  // The line ends of each 64-byte block are found at once and the lines taken
  // from them bit by bit, so a line's start does not wait on the parse of the
  // line before and the core overlaps several lines. A line is read from both
  // ends: the method and the name from its start, the latency (at most 10
  // digits after the last space) and the status back from its LF.
  void run(const char *p, const char *end) {
    const __m256i space = _mm256_set1_epi8(' '), lf = _mm256_set1_epi8('\n');
    const __m128i space16 = _mm256_castsi256_si128(space);
    u32 *c = nullptr, *lim = nullptr;
    int h = -1;
    u64 stamp = 0, at;
    const char *block = p;
    for (u64 ends = eq64(load(block), load(block + 32), lf, lf); p < end;
         ends &= ends - 1) {
      while (!ends) {
        // The log streams through once; fetching it 1 KB ahead with the
        // non-temporal hint keeps it from pushing the endpoint table and the
        // names out of the core's caches.
        _mm_prefetch(block + 1024, _MM_HINT_NTA);
        block += 64, ends = eq64(load(block), load(block + 32), lf, lf);
      }
      const char *e = block + __builtin_ctzll(ends);
      memcpy(&at, p + 5, 8);
      if (at != stamp) [[unlikely]] {
        if (h >= 0)
          cur[h] = c, stop[h] = lim, --active[h];
        int day = (p[8] - '0') * 10 + p[9] - '0',
            hr = (p[11] - '0') * 10 + p[12] - '0';
        h = (p[6] == '0' ? day - 30 : day + 1) * 24 + hr - 18, stamp = at;
        ++active[h];
        if (closed[h]) // too late for this pass
          return late.store(true);
        c = cur[h], lim = stop[h];
      }
      u32 method = _mm_movemask_epi8(
          _mm_cmpeq_epi8(_mm_loadu_si128((const __m128i *)(p + 28)), space16));
      u32 tail = _mm_movemask_epi8(
          _mm_cmpeq_epi8(_mm_loadu_si128((const __m128i *)(e - 16)), space16));
      const char *name = p + 29 + __builtin_ctz(method);
      u32 len = __builtin_clz(tail) - 16; // digits after the last space
      const char *ne = e - len - 5;       // the space before the status
      u32 n = ne - name;
      // Up to 8 digits are decoded in one word: the 8 bytes before the LF,
      // with those below the digits cleared so they read as leading zeros,
      // combined 2, 4 and 8 at a time with multiplies. Longer ones (rare) go
      // digit by digit.
      u32 lat = 0;
      if (len <= 8) [[likely]] {
        u64 digits;
        memcpy(&digits, e - 8, 8);
        digits =
            digits >> (64 - 8 * len) << (64 - 8 * len) & 0x0F0F0F0F0F0F0F0F;
        digits = (digits * 2561) >> 8 & 0x00FF00FF00FF00FF;
        digits = (digits * 6553601) >> 16 & 0x0000FFFF0000FFFF;
        lat = (digits * 42949672960001) >> 32;
      } else
        for (const char *d = e - len; d < e; ++d)
          lat = lat * 10 + *d - '0';
      u32 key = (endpoints.find(name, n, load(name), load(name + 32)) + 1) |
                (ne[1] == '5' ? ERR : 0);
      p = e + 1;
      if (lat >= PACKED_MAX) [[unlikely]] {
        if (lim - c < 2) // the pair stays in one block
          c = lim;
        if (c == lim)
          c = claim(h), lim = c + BLOCK;
        *c++ = ESCAPE | key, *c++ = lat;
        continue;
      }
      if (c == lim) [[unlikely]]
        c = claim(h), lim = c + BLOCK;
      *c++ = lat << KEY_BITS | key;
    }
    if (h >= 0)
      cur[h] = c, stop[h] = lim, --active[h];
  }

  static u32 *claim(int h) {
    return records + h * per_hour + filled[h].fetch_add(BLOCK);
  }
};

int main() {
  prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0);
  const int nt = std::clamp<int>(std::thread::hardware_concurrency(), 1, 12);
  int in = open("/data/requests.log", O_RDONLY);
  int out = open("/out/summary.txt", O_RDWR | O_CREAT | O_TRUNC, 0644);
  if (in < 0 || out < 0)
    return perror("open"), 1;

  size_t size = lseek(in, 0, SEEK_END);
  auto *log =
      (char *)mmap(nullptr, size + 4096, 1, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (size && mmap(log, size, 1, MAP_PRIVATE | MAP_FIXED, in, 0) == MAP_FAILED)
    return perror("mmap"), 1;

  // An hour holds at most two places for every line (one is at least 40
  // bytes), a part block per thread, and after them its sorted keys, one a
  // line; only the pages written are backed.
  per_hour = (size / 13 / BLOCK + nt + 2) * BLOCK;
  records = (u32 *)mmap(nullptr, HOURS * per_hour * 4, 3,
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  // Pieces start at line starts; first[k] is the hour of piece k's first line.
  const int pieces = 8 * nt;
  std::vector<size_t> cut(pieces + 1, size);
  std::vector<int> first(pieces + 1, HOURS);
  cut[0] = 0;
  for (int k = 1; k < pieces; ++k)
    for (cut[k] = std::max(cut[k - 1], size * k / pieces);
         cut[k] < size && log[cut[k] - 1] != '\n';)
      ++cut[k];
  for (int k = 0; k < pieces; ++k)
    if (const char *p = log + cut[k]; cut[k] < size) {
      int day = (p[8] - '0') * 10 + p[9] - '0';
      first[k] = (p[6] == '0' ? day - 30 : day + 1) * 24 + (p[11] - '0') * 10 +
                 p[12] - '0' - 18;
    }

  // Hour h's count[h] lines sit from lines + h * hour_bytes, each hour on
  // pages of its own, handed back once the hour's text is written.
  struct Line {
    u32 id, n, p50, p95, p99, errors;
  };
  const size_t hour_bytes = (ID_MASK * sizeof(Line) + 4095) & ~size_t(4095);
  char *lines =
      (char *)mmap(nullptr, HOURS * hour_bytes, 3,
                   MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  std::vector<u32> count(HOURS);
  std::vector<u64> at(HOURS + 1);
  auto ppm = [](const Line &l) { return u32(u64(l.errors) * 1000000 / l.n); };

  std::mutex lock;
  std::vector<std::unique_ptr<Ranking>> rankings;
  auto ranking = [&]() -> const Ranking & {
    std::lock_guard hold(lock);
    if (rankings.empty() || rankings.back()->ids != endpoints.ids)
      rankings.emplace_back(new Ranking);
    return *rankings.back();
  };

  // Hour h, closed, is sorted by (endpoint rank, latency), the rank above a
  // 20-bit latency in one 31-bit key, by a radix sort of three passes (10, 10
  // and 11 bits) between its records and the unused rest of its region. Every
  // group then comes out sorted at a known place, and its percentiles are
  // read off by index. Latencies too large for 20 bits sort last in their
  // group with the same key; their values are kept, sorted, in a short list.
  // Finish an hour by grouping packed latencies by endpoint, then selecting
  // only the three nearest-rank percentiles in each group. This replaces the
  // three full-array radix passes with one endpoint scatter and in-place
  // introselect. Escape latencies stay in the small separately sorted list.
  auto finish = [&](int h, std::vector<u64> &big) {
    if (active[h]) // a thread is filing into it: too late for this pass
      return late.store(true);
    const Ranking &rank = ranking();
    const u32 ne = rank.by_rank.size();
    u32 *keys = records + h * per_hour, *sorted = keys + filled[h],
        total = 0;
    u32 normal_end[ID_MASK + 1] = {}, counts[ID_MASK + 1] = {},
        errors[ID_MASK + 1] = {};
    big.clear();
    for (u32 *r = keys; r < sorted; ++r) {
      u32 x = *r;
      if (!(x & ID_MASK)) // a place never written, at the end of a block
        continue;
      u32 e = rank.rank_of[x & ID_MASK];
      ++counts[e];
      errors[e] += (x & ERR) != 0;
      if (x >> KEY_BITS == PACKED_MAX) [[unlikely]] {
        big.push_back(u64(e) << 32 | *++r);
      } else {
        ++normal_end[e];
        keys[total++] = e << 20 | x >> KEY_BITS;
      }
    }

    // Stable grouping by endpoint. normal_end becomes each endpoint's
    // exclusive end offset in the scratch array.
    for (u32 e = 0, sum = 0; e < ne; ++e)
      sum += std::exchange(normal_end[e], sum);
    for (u32 j = 0; j < total; ++j) {
      u32 x = keys[j];
      sorted[normal_end[x >> 20]++] = x;
    }

    std::sort(big.begin(), big.end());
    u64 sz = 0;
    for (u32 e = 0, from = 0, b = 0; e < ne; from = normal_end[e++]) {
      u32 normal_n = normal_end[e] - from, n = counts[e];
      if (!n)
        continue;
      u32 large = 0;
      while (b + large < big.size() && (big[b + large] >> 32) == e)
        ++large;

      // Nearest rank p is index ceil(p * n / 100) - 1. Select only
      // requested normal ranks, reusing each partition for the next rank.
      u32 k[3] = {(50 * n + 99) / 100 - 1,
                  (95 * n + 99) / 100 - 1,
                  (99 * n + 99) / 100 - 1};
      u32 v[3];
      int last = -1;
      for (int i = 0; i < 3; ++i) {
        if (k[i] < normal_n) {
          if (int(k[i]) > last) {
            auto first = sorted + from + last + 1;
            auto nth = sorted + from + k[i];
            auto end = sorted + from + normal_n;
            std::nth_element(first, nth, end);
            last = k[i];
          }
          v[i] = sorted[from + k[i]] & PACKED_MAX;
        } else {
          v[i] = u32(big[b + k[i] - normal_n]);
        }
      }
      b += large;
      Line &l = ((Line *)(lines + h * hour_bytes))[count[h]++];
      l = {rank.by_rank[e], n, v[0], v[1], v[2], errors[e]};
      sz += 28 + endpoints.length[l.id];
      for (u32 x : {l.n, l.p50, l.p95, l.p99, l.errors, ppm(l)})
        sz += width(x);
    }
    madvise(keys, (sorted + total - keys) * 4, MADV_DONTNEED);
    at[h] = sz;
  };

  // The pass. Each thread closes and finishes the next hour once the first
  // piece not yet parsed starts two hours after it (or every piece is
  // parsed, if a pass was too late), and otherwise parses the next piece.
  std::vector<Parser> parsers(nt);
  std::vector<char> parsed(pieces + 1);
  int front = 0, next_piece = 0, next_hour = 0, passes = 1;
  bool stream = true;
  // If the pass was too late, everything is handed back and it is redone.
  std::barrier passed(nt, [&]() noexcept {
    if (!late)
      return;
    madvise(records, HOURS * per_hour * 4, MADV_DONTNEED);
    madvise(lines, HOURS * hour_bytes, MADV_DONTNEED);
    for (int h = 0; h < HOURS; ++h)
      filled[h] = active[h] = count[h] = 0, closed[h] = false;
    for (auto &p : parsers)
      p = Parser();
    std::fill(parsed.begin(), parsed.end(), 0);
    front = next_piece = next_hour = 0, stream = late = false, ++passes;
  });
  char *text = nullptr;
  // Every hour is sized: offsets, then the output mapped at its final size.
  std::barrier sized(nt, [&]() noexcept {
    u64 total = 0;
    for (int h = 0; h < HOURS; ++h)
      total += std::exchange(at[h], total);
    at[HOURS] = total;
    if (!total)
      return;
    if (ftruncate(out, total) ||
        (text = (char *)mmap(nullptr, total, 3, MAP_SHARED, out, 0)) ==
            MAP_FAILED)
      _exit(1);
  });

  std::vector<std::thread> pool;
  for (int t = 0; t < nt; ++t)
    pool.emplace_back([&, t] {
      std::vector<u64> big;
      for (int pass = 0; pass < passes; ++pass) {
        while (!late) {
          int h = -1, k = -1;
          {
            std::lock_guard hold(lock);
            while (front < pieces && parsed[front])
              ++front;
            if (next_hour < HOURS &&
                (front == pieces || (stream && first[front] >= next_hour + 2)))
              h = next_hour++, closed[h] = true;
            else if (next_piece < pieces)
              k = next_piece++;
            else if (next_hour == HOURS)
              break;
          }
          if (h >= 0)
            finish(h, big);
          else if (k >= 0) {
            parsers[t].run(log + cut[k], log + cut[k + 1]);
            uintptr_t a = (uintptr_t(log + cut[k]) + 4095) & ~uintptr_t(4095),
                      b = uintptr_t(log + cut[k + 1]) & ~uintptr_t(4095);
            if (b > a)
              madvise((void *)a, b - a, MADV_DONTNEED);
            std::lock_guard hold(lock);
            parsed[k] = true;
          } else
            std::this_thread::yield();
        }
        passed.arrive_and_wait();
      }
      sized.arrive_and_wait();
      for (int h = t; h < HOURS; h += nt) {
        char label[32];
        int day = (h + 18) / 24;
        snprintf(label, 32, "2026-%02d-%02dT%02d:00:00Z ", day < 2 ? 10 : 11,
                 day < 2 ? 30 + day : day - 1, (h + 18) % 24);
        char *o = text + at[h];
        for (u32 i = 0; i < count[h]; ++i) {
          const Line &l = ((Line *)(lines + h * hour_bytes))[i];
          std::string_view name = endpoints.name(l.id);
          o = std::copy_n(label, 21, o);
          o = std::copy(name.begin(), name.end(), o);
          for (u32 x : {l.n, l.p50, l.p95, l.p99, l.errors, ppm(l)})
            *o = ' ', o += 1 + width(x), put(o, x);
          *o++ = '\n';
        }
        madvise(lines + h * hour_bytes, hour_bytes, MADV_DONTNEED);
      }
    });
  for (auto &th : pool)
    th.join();
  close(out);
}