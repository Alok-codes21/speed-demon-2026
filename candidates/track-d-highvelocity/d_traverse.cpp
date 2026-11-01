// Adapted from the MIT-licensed Track D implementation in
// insightlabs38-pixel/sd-sm (team HighVelocity), commit
// 650272f4043d367210da65af1ca0d8f07f640f5f, track-d/d_traverse.cpp.
// Copyright (c) 2026 Hackathon Raptors CIC and the contributors of that
// repository. Source: https://github.com/insightlabs38-pixel/sd-sm
// Verbatim apart from this attribution header; MIT license terms apply.
//
#include <algorithm>
#include <atomic>
#include <cstdint>
#include <cstring>
#include <emmintrin.h>
#include <fcntl.h>
#include <numeric>
#include <queue>
#include <string>
#include <sys/mman.h>
#include <sys/stat.h>
#include <thread>
#include <unistd.h>
#include <vector>

using u32 = uint32_t;
using u64 = uint64_t;
// Above any path length (all are below 2^53), so INF + d cannot wrap.
constexpr u64 INF = u64(1) << 62;
// Edge index -> weight too large for its packed field, sorted by index.
using Escapes = std::vector<std::pair<size_t, u64>>;
using Bits = std::vector<u64>;
bool test(const Bits &b, u32 v) { return b[v >> 6] >> (v & 63) & 1; }
void set(Bits &b, u32 v) { b[v >> 6] |= u64(1) << (v & 63); }
unsigned id_bits(u64 n) { return n < 2 ? 1 : 64 - __builtin_clzll(n - 1); }
u32 cut(size_t n, unsigned t, unsigned threads) { return n * t / threads; }

// Uninitialized storage, committed page by page as it is written. Huge pages
// are refused, as partly used buffers would round up to them.
template <class T> T *alloc(size_t n) {
  size_t bytes = n * sizeof(T) + 64;
  int flags = MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE;
  void *p = mmap(nullptr, bytes, PROT_READ | PROT_WRITE, flags, -1, 0);
  madvise(p, bytes, MADV_NOHUGEPAGE);
  return static_cast<T *>(p);
}
template <class T> void release(T *p, size_t n) { munmap(p, n * sizeof(T)); }

template <class F> void parallel(unsigned threads, F f) {
  std::vector<std::thread> pool;
  for (unsigned t = 1; t < threads; ++t)
    pool.emplace_back(f, t);
  f(0);
  for (auto &t : pool)
    t.join();
}

// ---------------------------------------------------------------- parsing

// Bit i is set when p[i] is a separator (space or LF), the only bytes below
// '0' in the input.
u64 separators(const char *p) {
  u64 m = 0;
  for (int k = 0; k < 64; k += 16) {
    __m128i x = _mm_loadu_si128(reinterpret_cast<const __m128i *>(p + k));
    u32 bits = _mm_movemask_epi8(_mm_cmplt_epi8(x, _mm_set1_epi8('0')));
    m |= u64(bits) << k;
  }
  return m;
}

// Value of the digits [a, b). Up to eight digits are converted at once from
// the 8 bytes ending at b, the bytes before a masked off.
u32 decimal(const char *a, const char *b, const char *file) {
  u32 len = b - a, v = 0;
  if (len > 8 || b - file < 8) {
    for (; a < b; ++a)
      v = v * 10 + (*a - '0');
    return v;
  }
  u64 x, m = 0x000000FF000000FF;
  memcpy(&x, b - 8, 8);
  x &= (~u64(0) << (64 - 8 * len)) & 0x0F0F0F0F0F0F0F0F;
  x = x * 10 + (x >> 8); // adjacent digit pairs
  x = (x & m) * 0x000F424000000064 + ((x >> 16) & m) * 0x0000271000000001;
  return x >> 32;
}

// Calls emit(u, v, w) for each line in [begin, end) of the file [lo, hi).
template <class Emit>
void parse(const char *lo, const char *hi, const char *begin, const char *end,
           Emit emit) {
  const char *field = begin;
  u32 val[3], k = 0;
  char pad[64];
  for (const char *q = begin; q < end; q += 64) {
    // The file's last block is scanned from a copy padded with digits.
    if (hi - q < 64)
      memset(pad, '0', 64), memcpy(pad, q, hi - q);
    u64 m = separators(hi - q < 64 ? pad : q);
    if (end - q < 64)
      m &= (u64(1) << (end - q)) - 1;
    for (; m; m &= m - 1) {
      const char *sep = q + __builtin_ctzll(m);
      val[k] = decimal(field, sep, lo), field = sep + 1;
      if (++k == 3)
        emit(val[0], val[1], val[2]), k = 0;
    }
  }
}

u32 scan(const char *&p) {
  u32 v = 0;
  while (*p >= '0')
    v = v * 10 + (*p++ - '0');
  return ++p, v;
}

// Maps a file read-only; its pages stay in the page cache.
const char *map(const std::string &path, size_t &size) {
  int fd = open(path.c_str(), O_RDONLY);
  struct stat st;
  if (fd < 0 || fstat(fd, &st))
    exit(2);
  size = st.st_size;
  void *p = mmap(nullptr, size + 1, PROT_READ, MAP_PRIVATE, fd, 0);
  return static_cast<const char *>(p);
}

// ---------------------------------------------------------------- graphs

// Compressed rows of packed edges, head << wbits | weight. A weight that does
// not fit leaves the field all ones and sits in the escape table.
struct Graph {
  u32 n, m, wbits, mask, *off, *adj;
  Escapes esc;
  Graph(u32 n, u32 m)
      : n(n), m(m), wbits(32 - id_bits(n + 1)), mask((1u << wbits) - 1),
        off(alloc<u32>(n + 1)), adj(alloc<u32>(m)) {}
  ~Graph() { release(off, n + 1), release(adj, m); }
  u32 head(u32 e) const { return e >> wbits; }
  u64 weight(size_t i) const {
    if ((adj[i] & mask) != mask)
      return adj[i] & mask;
    return std::lower_bound(esc.begin(), esc.end(), std::pair(i, 0ul))->second;
  }
  void put(size_t i, u32 v, u64 w, Escapes &fresh) {
    if (w >= mask)
      fresh.push_back({i, w});
    adj[i] = v << wbits | std::min<u64>(w, mask);
  }
  void merge(std::vector<Escapes> &parts) {
    for (Escapes &e : parts)
      esc.insert(esc.end(), e.begin(), e.end());
    std::sort(esc.begin(), esc.end());
  }
};

// An edge binned by its tail's bucket: the tail's low bits with the head, and
// 16 weight bits. Heavier weights keep all ones and go to a side table.
struct __attribute__((packed)) Entry {
  u32 key;
  uint16_t w;
};

// One worker's edges grouped by their tail's bucket of ids, in chunks. The
// workers share a pool laid out by group of buckets, so a group's entries are
// released together once its rows are built, and the bins shrink while the
// graph grows. Each worker owns `cap` entries (whole pages) of each group.
struct alignas(64) Bins {
  static constexpr u32 CHUNK = 256, GROUP = 16;
  u32 bits, shift, buckets, cap;
  Entry *pool;
  std::vector<u32> fill, used;
  std::vector<std::vector<u32>> chunks;
  std::vector<std::pair<u32, u64>> heavy; // pool index -> weight
  Bins(u32 n, u32 cap, Entry *pool, unsigned t, unsigned workers)
      : bits(id_bits(n)), shift(std::max(6u, std::min(11u, 32 - bits))),
        buckets(n >> shift), cap(cap), pool(pool), fill(buckets + 1),
        chunks(buckets + 1) {
    for (u32 g = 0; g <= buckets / GROUP; ++g)
      used.push_back((g * workers + t) * cap);
  }
  void add(u32 u, u32 v, u64 w) {
    u32 b = u >> shift;
    if (fill[b] % CHUNK == 0)
      chunks[b].push_back(fill[b] = used[b / GROUP]), used[b / GROUP] += CHUNK;
    if (w >= 0xFFFF)
      heavy.push_back({fill[b], w});
    u32 key = (u & ((1u << shift) - 1)) << bits | v;
    pool[fill[b]++] = {key, uint16_t(std::min<u64>(w, 0xFFFF))};
  }
  u32 size(u32 b) const {
    return chunks[b].size() * CHUNK - (CHUNK - fill[b] % CHUNK) % CHUNK;
  }
  template <class F> void each(u32 b, F f) const {
    for (u32 c : chunks[b])
      for (u32 i = c; i < std::min(c + CHUNK, fill[b]); ++i)
        f(pool[i], i);
  }
};

// Bins for each worker, any of which may receive `most` edges.
std::vector<Bins> make_bins(u32 n, size_t most, unsigned threads) {
  u32 cap = (most / 2048 + 9) * 2048;
  std::vector<Bins> bins;
  for (unsigned t = 0; t < threads; ++t)
    bins.emplace_back(n, cap, nullptr, t, threads);
  size_t groups = bins[0].buckets / Bins::GROUP + 1;
  Entry *pool = alloc<Entry>(groups * threads * cap);
  for (Bins &bin : bins)
    bin.pool = pool;
  return bins;
}

// Builds the rows of each bucket of tails in cache, then releases the bins of
// finished groups. mark(u, degree) sees each vertex's out-degree.
template <class Mark>
Graph *build(std::vector<Bins> &bins, u32 n, u32 m, unsigned threads,
             Mark mark) {
  Graph *g = new Graph(n, m);
  const Bins &shape = bins[0];
  u32 bits = shape.bits, shift = shape.shift, buckets = shape.buckets;
  for (Bins &bin : bins)
    std::sort(bin.heavy.begin(), bin.heavy.end());
  // Bucket b's edges start after those of the buckets before it.
  std::vector<u32> start(buckets + 2, 0);
  for (u32 b = 0; b <= buckets; ++b)
    for (const Bins &bin : bins)
      start[b + 1] += bin.size(b);
  std::partial_sum(start.begin(), start.end(), start.begin());
  std::vector<std::atomic<u32>> left(buckets / Bins::GROUP + 1);
  for (u32 b = 0; b <= buckets; ++b)
    ++left[b / Bins::GROUP]; // buckets not yet built
  std::vector<Escapes> escapes(threads);
  std::atomic<u32> next{0};
  g->off[0] = 0;
  parallel(threads, [&](unsigned t) {
    std::vector<u32> at(1u << shift);
    for (u32 b; (b = next++) <= buckets;) {
      u32 first = b << shift, size = std::min(n - first, 1u << shift);
      std::fill(at.begin(), at.end(), 0);
      for (const Bins &bin : bins)
        bin.each(b, [&](Entry e, u32) { ++at[e.key >> bits]; });
      for (u32 x = 0, pos = start[b]; x < size; ++x) {
        mark(first + x, at[x]);
        g->off[first + x + 1] = pos += at[x], at[x] = pos - at[x];
      }
      for (const Bins &bin : bins)
        bin.each(b, [&](Entry e, u32 i) {
          u64 w = e.w;
          if (w == 0xFFFF)
            w = std::lower_bound(bin.heavy.begin(), bin.heavy.end(),
                                 std::pair(i, u64(0)))
                    ->second;
          g->put(at[e.key >> bits]++, e.key & ((1u << bits) - 1), w,
                 escapes[t]);
        });
      if (--left[b / Bins::GROUP] == 0)
        release(shape.pool + size_t(b / Bins::GROUP) * threads * shape.cap,
                threads * shape.cap);
    }
  });
  g->merge(escapes);
  return g;
}

// Parses the edges into per-worker bins and builds the raw graph. Marks the
// vertices that can be bypassed: out-degree <= 2 and no query endpoint.
Graph *read_graph(const char *lo, const char *hi, const Bits &terminal,
                  Bits &bypass, unsigned threads) {
  const char *p = lo;
  u32 n = scan(p), m = scan(p);
  // Worker k parses from the first line start at or after its share's start.
  auto line = [&](unsigned k) {
    const char *x = p + (hi - p) * k / threads;
    return x == p || x == hi ? x : (const char *)memchr(x, '\n', hi - x) + 1;
  };
  size_t most = 0;
  for (unsigned t = 0; t < threads; ++t)
    most = std::max<size_t>(most, (line(t + 1) - line(t)) / 6);
  std::vector<Bins> bins = make_bins(n, most, threads);
  parallel(threads, [&](unsigned t) {
    Bins &bin = bins[t];
    parse(lo, hi, line(t), line(t + 1),
          [&](u32 u, u32 v, u32 w) { bin.add(u, v, w); });
  });
  return build(bins, n, m, threads, [&](u32 u, u32 degree) {
    if (degree <= 2 && !test(terminal, u))
      set(bypass, u); // buckets are word aligned
  });
}

// ---------------------------------------------------------------- ordering

// One worker's share of the search order: vertices in discovery order, the
// end of each one's out-edges, and those edges with their raw heads.
struct alignas(64) Region {
  u32 *order, *ends, *out, len = 0, edges = 0;
  Escapes esc; // keyed by position in out
};

// Grows breadth-first regions over the raw graph. Expanding a vertex follows
// each out-edge through bypassable vertices (one way on, never back) to the
// first survivor, so degree-2 contraction, locality ordering and gathering
// share one pass. The walks of a batch advance one hop per round, so their
// cache misses overlap.
struct Grower {
  static constexpr u32 BATCH = 8;
  struct Walk {
    u32 pos, root, prev, at, steps;
    u64 w;
  };
  const Graph &raw;
  const Bits &bypass;
  Bits &claimed, mine;
  Region &reg;
  std::vector<Walk> walks;
  u64 sink = 0;

  // Claims race without locked instructions: a vertex claimed twice leaves a
  // copy that no edge reaches, as every head resolves through one map entry.
  // The private bitmap answers most checks, as a region mostly meets itself.
  void claim(u32 z) {
    u64 bit = u64(1) << (z & 63), *word = &claimed[z >> 6];
    if (mine[z >> 6] & bit)
      return;
    mine[z >> 6] |= bit;
    u64 seen = __atomic_load_n(word, __ATOMIC_RELAXED);
    __atomic_store_n(seen & bit ? &sink : word, seen | bit, __ATOMIC_RELAXED);
    reg.order[reg.len] = z, reg.len += !(seen & bit);
  }
  void put(u32 pos, u32 z, u64 w) {
    if (w >= raw.mask)
      reg.esc.push_back({pos, w});
    reg.out[pos] = z << raw.wbits | std::min<u64>(w, raw.mask);
  }
  // Advances a walk by one hop; returns false once its edge is final. A
  // branch ends the walk at a survivor; a dead end or a way back to the root
  // leaves a useless self-loop.
  bool step(Walk &w) {
    u32 ahead = 0, next = w.at, i = raw.off[w.at], which = i;
    for (; i < raw.off[w.at + 1]; ++i) {
      u32 h = raw.head(raw.adj[i]);
      if (h != w.prev && h != w.at)
        ++ahead, next = h, which = i;
    }
    if (ahead > 1) {
      next = w.at;
    } else if (ahead == 0 || next == w.root) {
      next = w.root, w.w = 0;
    } else {
      w.w += raw.weight(which), w.prev = w.at, w.at = next;
      if (test(bypass, next) && ++w.steps < 64)
        return __builtin_prefetch(&raw.off[next]), true;
    }
    if (next != w.root)
      claim(next);
    put(w.pos, next, w.w);
    return false;
  }
  void expand(u32 u, size_t &walking) {
    for (u32 i = raw.off[u]; i < raw.off[u + 1]; ++i) {
      u32 y = raw.head(raw.adj[i]), pos = reg.edges;
      if (y == u)
        continue;
      ++reg.edges;
      if (!test(bypass, y)) {
        claim(y), put(pos, y, raw.weight(i));
        continue;
      }
      if (walks.size() == walking)
        walks.resize(2 * walking + 64);
      walks[walking++] = {pos, u, u, y, 0, raw.weight(i)};
      __builtin_prefetch(&raw.off[y]);
    }
  }
  // Grows a region from z unless z is already claimed. Rows are prefetched
  // two batches ahead: first the offsets, then the edges.
  void grow(u32 z) {
    u32 lo = reg.len, hi;
    for (claim(z); lo < reg.len; lo = hi) {
      hi = std::min(reg.len, lo + BATCH);
      for (u32 h = hi; h < std::min(reg.len, hi + BATCH); ++h)
        __builtin_prefetch(&raw.adj[raw.off[reg.order[h]]]);
      for (u32 h = hi + BATCH; h < std::min(reg.len, hi + 2 * BATCH); ++h)
        __builtin_prefetch(&raw.off[reg.order[h]]);
      size_t walking = 0;
      for (u32 h = lo; h < hi; ++h)
        expand(reg.order[h], walking), reg.ends[h] = reg.edges;
      while (walking) {
        size_t kept = 0;
        for (size_t i = 0; i < walking; ++i)
          __builtin_prefetch(&raw.adj[raw.off[walks[i].at]]);
        for (size_t i = 0; i < walking; ++i)
          if (step(walks[i]))
            walks[kept++] = walks[i];
        walking = kept;
      }
    }
  }
};

// Grows the regions from the survivors of each worker's id slice, then
// concatenates them into the forward graph and renames the query endpoints.
Graph *contract(Graph *raw, const Bits &bypass, unsigned threads,
                std::vector<u32> &ends) {
  u32 n = raw->n, m = raw->off[n], lbits = id_bits(n + 1);
  Bits claimed(bypass.size());
  std::vector<Region> regions(threads);
  parallel(threads, [&](unsigned t) {
    Region &reg = regions[t];
    reg.order = alloc<u32>(n), reg.ends = alloc<u32>(n),
    reg.out = alloc<u32>(m);
    Grower grower{*raw, bypass, claimed, Bits(claimed.size()), reg};
    for (u32 x = cut(n, t, threads); x < cut(n, t + 1, threads); ++x)
      if (!test(bypass, x))
        grower.grow(x);
  });
  u32 wbits = raw->wbits, mask = raw->mask;
  delete raw;
  // Raw id -> region << lbits | position. A vertex claimed twice is named by
  // the entry that lands last, the same for every head.
  u32 *where = alloc<u32>(n);
  parallel(threads, [&](unsigned t) {
    for (u32 i = 0; i < regions[t].len; ++i)
      __atomic_store_n(&where[regions[t].order[i]], t << lbits | i,
                       __ATOMIC_RELAXED);
    release(regions[t].order, n);
  });
  std::vector<u32> base(threads + 1, 0), edges(threads + 1, 0);
  for (unsigned t = 0; t < threads; ++t) {
    base[t + 1] = base[t] + regions[t].len;
    edges[t + 1] = edges[t] + regions[t].edges;
  }
  auto locate = [&](u32 r) {
    return base[r >> lbits] + (r & ((1u << lbits) - 1));
  };
  Graph *g = new Graph(base[threads], edges[threads]);
  std::vector<Escapes> escapes(threads);
  parallel(threads, [&](unsigned t) {
    Region &reg = regions[t];
    std::sort(reg.esc.begin(), reg.esc.end());
    auto esc = reg.esc.begin();
    for (u32 i = 0, pos = 0; i < reg.len; ++i) {
      g->off[base[t] + i] = edges[t] + pos;
      for (; pos < reg.ends[i]; ++pos) {
        u32 e = reg.out[pos], w = e & mask;
        u64 weight = w == mask ? esc++->second : w;
        g->put(edges[t] + pos, locate(where[e >> wbits]), weight, escapes[t]);
      }
    }
    release(reg.ends, n), release(reg.out, m);
  });
  g->off[g->n] = edges[threads];
  g->merge(escapes);
  for (u32 &v : ends)
    v = locate(where[v]);
  release(where, n);
  return g;
}

// Transposes g by binning its edges by head, as the parser bins its input.
Graph *transpose(const Graph &g, unsigned threads) {
  u32 most = 0;
  for (unsigned t = 0; t < threads; ++t)
    most = std::max(most, g.off[cut(g.n, t + 1, threads)] -
                              g.off[cut(g.n, t, threads)]);
  std::vector<Bins> bins = make_bins(g.n, most, threads);
  parallel(threads, [&](unsigned t) {
    for (u32 u = cut(g.n, t, threads); u < cut(g.n, t + 1, threads); ++u)
      for (u32 i = g.off[u]; i < g.off[u + 1]; ++i)
        bins[t].add(g.head(g.adj[i]), u, g.weight(i));
  });
  return build(bins, g.n, g.m, threads, [](u32, u32) {});
}

// ---------------------------------------------------------------- queries

// Monotone queue: a ring of buckets 2^shift wide; larger jumps wait in a heap.
// push always writes the entry but counts it only when `take`, so relaxations
// need no branch.
struct Queue {
  static constexpr u32 B = 2048;
  struct Bucket {
    u32 *data = nullptr, size = 0, cap = 0;
  } bucket[B];
  u64 occupied[B / 64] = {}, cur = 0, near = 0;
  u32 shift, node_bits;
  using Far = std::pair<u64, u32>;
  std::priority_queue<Far, std::vector<Far>, std::greater<Far>> far;
  explicit Queue(u32 n) : shift(std::min(7u, 32 - id_bits(n))) {
    node_bits = 32 - shift;
  }
  void clear() {
    for (u32 w = 0; w < B / 64; occupied[w++] = 0)
      for (u64 bits = occupied[w]; bits; bits &= bits - 1)
        bucket[w * 64 + __builtin_ctzll(bits)].size = 0;
    far = {}, cur = near = 0;
  }
  void push(u64 d, u32 v, bool take = true) {
    if ((d >> shift) - cur >= B) {
      if (take)
        far.push({d, v});
      return;
    }
    u32 b = (d >> shift) % B;
    Bucket &k = bucket[b];
    if (k.size == k.cap)
      k.data = (u32 *)realloc(k.data, (k.cap = std::max(16u, 2 * k.cap)) * 4);
    k.data[k.size] = v | u32(d & ((1u << shift) - 1)) << node_bits;
    k.size += take, near += take;
    occupied[b / 64] |= u64(take) << b % 64;
  }
  // Moves cur to the first nonempty bucket and returns its least possible
  // key; INF when the queue is empty.
  u64 lower_bound() {
    if (near) {
      u32 b = cur % B, w = b / 64;
      u64 bits = occupied[w] & (~u64(0) << b % 64);
      while (!bits)
        bits = occupied[w = (w + 1) % (B / 64)];
      cur += (w * 64 + __builtin_ctzll(bits) - b) % B;
    } else if (!far.empty()) {
      cur = far.top().first >> shift;
    } else {
      return INF;
    }
    for (; !far.empty() && (far.top().first >> shift) - cur < B; far.pop())
      push(far.top().first, far.top().second);
    return cur << shift;
  }
};

// Labels in pages of 128, materialized on first write and recycled by the
// next search, so memory follows the searches rather than the graph. Pages
// are handed out in order: only the deepest search's pages are ever touched.
// A blank label holds NONE: INF, or all ones when L is narrower.
template <class L> struct Labels {
  static constexpr L NONE = std::min<u64>(INF, L(-1));
  std::vector<L *> dir;
  std::vector<u32> used;
  L *store, blank[128];
  explicit Labels(u32 n) : dir(n / 128 + 1, blank), store(alloc<L>(n + 128)) {
    std::fill(blank, blank + 128, NONE);
  }
  L *at(u32 v) const { return dir[v / 128] + v % 128; }
  u64 get(u32 v) const { return *at(v); }
  // Any finite distance improves a blank label, so writes go to real pages.
  L *slot(u32 v) {
    if (dir[v / 128] == blank) {
      dir[v / 128] = std::fill_n(store + used.size() * 128, 128, NONE) - 128;
      used.push_back(v / 128);
    }
    return at(v);
  }
  void reset() {
    for (u32 page : used)
      dir[page] = blank;
    used.clear();
  }
};

// One direction of a search with labels of type L. A distance that does not
// fit L sets `wide`, which voids the search. With narrow labels a blank one
// reads as 2^32 - 1, so nd + other(v) stays a path length or at least that.
template <class L> struct Side {
  const Graph &g;
  Labels<L> dist;
  Queue queue;
  bool wide = false;
  explicit Side(const Graph &g) : g(g), dist(g.n), queue(g.n) {}
  void start(u32 s) {
    dist.reset(), queue.clear();
    *dist.slot(s) = 0, queue.push(0, s);
  }
  // Settles the current bucket in FIFO order, prefetching rows and labels of
  // the entries ahead; returns the number of vertices settled. Relaxations are
  // branch-free: nd + other(v) is a real path length either way.
  size_t drain(const Side &other, u64 &best) {
    Queue::Bucket &bk = queue.bucket[queue.cur % Queue::B];
    u32 mask = (1u << queue.node_bits) - 1, settled = 0;
    bool overflow = false;
    for (u32 k = 0; k < bk.size; ++k) {
      u32 ahead = bk.data[std::min(k + 8, bk.size - 1)] & mask;
      __builtin_prefetch(&g.off[ahead]), __builtin_prefetch(dist.at(ahead));
      ahead = bk.data[std::min(k + 4, bk.size - 1)] & mask;
      __builtin_prefetch(&g.adj[g.off[ahead]]);
      u32 u = bk.data[k] & mask;
      u64 d = queue.cur << queue.shift | bk.data[k] >> queue.node_bits;
      if (dist.get(u) != d)
        continue;
      ++settled;
      for (u32 i = g.off[u]; i < g.off[u + 1]; ++i) {
        u32 v = g.head(g.adj[i]);
        L *x = dist.slot(v);
        u64 nd = d + g.weight(i), old = *x;
        *x = L(std::min(nd, old));
        overflow |= nd >= Labels<L>::NONE;
        queue.push(nd, v, nd < old);
        best = std::min(best, nd + other.dist.get(v));
      }
    }
    queue.near -= bk.size, bk.size = 0;
    queue.occupied[queue.cur % Queue::B / 64] &= ~(u64(1) << queue.cur % 64);
    wide |= overflow;
    return settled;
  }
};

// An exact forward probe settles up to 256 vertices; then bidirectional
// Dijkstra balances settled counts until the two queue fronts sum to at least
// the best meeting. Returns the distance, or NONE or more if t is unreached
// as far as L can tell.
template <class L> struct Worker {
  Side<L> fwd, bwd;
  u64 solve(u32 s, u32 t) {
    fwd.wide = bwd.wide = false;
    if (s == t)
      return 0;
    if (fwd.g.off[s] == fwd.g.off[s + 1] || bwd.g.off[t] == bwd.g.off[t + 1])
      return INF;
    bwd.dist.reset(), fwd.start(s);
    u64 best = INF, fs = 0, bs = 0;
    while (fs < 256) {
      u64 f = fwd.queue.lower_bound(), known = fwd.dist.get(t);
      if (known < f || f == INF)
        return known;
      fs += fwd.drain(bwd, best);
    }
    bwd.start(t);
    best = fwd.dist.get(t);
    for (;;) {
      u64 f = fwd.queue.lower_bound(), b = bwd.queue.lower_bound();
      if (f == INF || b == INF || f + b >= best)
        return best;
      if (fs <= bs)
        fs += fwd.drain(bwd, best);
      else
        bs += bwd.drain(fwd, best);
    }
  }
};

int main() {
  const char *env_data = getenv("SD_DATA"), *env_out = getenv("SD_OUT");
  std::string data = env_data ? env_data : "/data";
  std::string out = env_out ? env_out : "/out";
  unsigned threads = std::min(12u, std::thread::hardware_concurrency());
  if (getenv("SD_THREADS"))
    threads = atoi(getenv("SD_THREADS"));
  threads = std::max(1u, threads);
  size_t qsize, gsize;
  const char *q = map(data + "/queries.txt", qsize), *qend = q + qsize;
  std::vector<u32> ends; // s0 t0 s1 t1 ...
  while (q < qend)
    ends.push_back(scan(q));
  const char *graph = map(data + "/graph.txt", gsize), *p = graph;
  Bits terminal(scan(p) / 64 + 1), bypass(terminal.size());
  for (u32 v : ends)
    set(terminal, v);
  Graph *raw = read_graph(graph, graph + gsize, terminal, bypass, threads);
  Graph *fwd = contract(raw, bypass, threads, ends),
        *rev = transpose(*fwd, threads);
  size_t queries = ends.size() / 2;
  std::vector<int64_t> answers(queries);
  std::vector<u32> by_source(queries);
  std::iota(by_source.begin(), by_source.end(), 0);
  std::sort(by_source.begin(), by_source.end(),
            [&](u32 a, u32 b) { return ends[2 * a] < ends[2 * b]; });
  std::atomic<size_t> next{0};
  parallel(std::min<size_t>(threads, queries + 1), [&](unsigned) {
    // 32-bit labels, unless a query's distances need all 64 bits.
    Worker<u32> narrow{Side<u32>(*fwd), Side<u32>(*rev)};
    Worker<u64> *wide = nullptr;
    for (size_t k; (k = next++) < queries;) {
      u32 i = by_source[k], s = ends[2 * i], t = ends[2 * i + 1];
      u64 d = narrow.solve(s, t);
      if (narrow.fwd.wide || narrow.bwd.wide || d >= Labels<u32>::NONE) {
        if (!wide)
          wide = new Worker<u64>{Side<u64>(*fwd), Side<u64>(*rev)};
        d = wide->solve(s, t);
      }
      answers[i] = d < INF ? int64_t(d) : -1;
    }
  });
  std::string text;
  for (int64_t x : answers)
    text += std::to_string(x) + '\n';
  int fd = open((out + "/distances.txt").c_str(), O_WRONLY | O_CREAT | O_TRUNC,
                0644);
  if (fd < 0 || write(fd, text.data(), text.size()) != ssize_t(text.size()))
    exit(2);
  exit(0); // leaves the large buffers to the kernel
}
