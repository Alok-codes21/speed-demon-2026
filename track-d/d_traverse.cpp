#include <algorithm>
#include <atomic>
#include <charconv>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fcntl.h>
#include <limits>
#include <queue>
#include <string>
#include <thread>
#include <type_traits>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <vector>

using u32 = uint32_t;
using u64 = uint64_t;
static constexpr u64 INF = std::numeric_limits<u64>::max() / 4;

[[noreturn]] static void die(const char *s) {
  std::fprintf(stderr, "track-d: %s\n", s);
  std::exit(2);
}

struct Mapping {
  const char *data = nullptr;
  size_t size = 0;
  int fd = -1;
  explicit Mapping(const std::string &path) {
    fd = ::open(path.c_str(), O_RDONLY);
    if (fd < 0) die("cannot open input");
    struct stat st {};
    if (::fstat(fd, &st) || st.st_size < 0) die("cannot stat input");
    size = static_cast<size_t>(st.st_size);
    if (size) {
      void *p = ::mmap(nullptr, size, PROT_READ, MAP_PRIVATE, fd, 0);
      if (p == MAP_FAILED) die("cannot mmap input");
      data = static_cast<const char *>(p);
      ::madvise(const_cast<char *>(data), size, MADV_SEQUENTIAL);
    }
  }
  Mapping(const Mapping &) = delete;
  Mapping &operator=(const Mapping &) = delete;
  ~Mapping() {
    if (data) ::munmap(const_cast<char *>(data), size);
    if (fd >= 0) ::close(fd);
  }
};

struct Scanner {
  const char *p, *end;
  explicit Scanner(const Mapping &m) : p(m.data), end(m.data + m.size) {}
  bool next(u32 &x) {
    while (p < end && static_cast<unsigned char>(*p) <= ' ') ++p;
    if (p == end) return false;
    u64 v = 0;
    while (p < end && *p >= '0' && *p <= '9') {
      v = v * 10 + static_cast<unsigned>(*p - '0');
      if (v > std::numeric_limits<u32>::max()) die("integer outside uint32 range");
      ++p;
    }
    if (p < end && static_cast<unsigned char>(*p) > ' ') die("invalid input token");
    x = static_cast<u32>(v);
    return true;
  }
  u32 required() {
    u32 x;
    if (!next(x)) die("unexpected end of input");
    return x;
  }
};

struct Edge {
  u32 to;
  u32 weight;
};
static_assert(sizeof(Edge) == 8);

struct CSR {
  u32 n = 0;
  std::vector<u64> off;
  std::vector<Edge> edges;

  template<class F>
  inline void each(u32 u, F &&f) const {
    for (u64 i = off[u], e = off[u + 1]; i < e; ++i)
      f(edges[static_cast<size_t>(i)]);
  }
};

struct Query {
  u32 s, t;
};

static std::vector<Query> read_queries(const std::string &path, u32 n) {
  Mapping m(path);
  Scanner sc(m);
  std::vector<Query> q;
  q.reserve(2048);
  u32 a, b;
  while (sc.next(a)) {
    if (!sc.next(b)) die("query file has odd number of integers");
    if (a >= n || b >= n) die("query vertex outside graph");
    q.push_back({a, b});
  }
  if (q.size() != 2000) die("expected exactly 2000 queries");
  return q;
}

static void build_graph(const std::string &path, CSR &fwd, CSR &rev) {
  Mapping m(path);
  Scanner sc(m);
  u32 n = sc.required();
  u32 declared_m = sc.required();
  if (n == 0) die("empty graph");
  fwd.n = rev.n = n;

  std::vector<u32> src, dst, wgt;
  src.reserve(declared_m);
  dst.reserve(declared_m);
  wgt.reserve(declared_m);
  std::vector<u64> outdeg(static_cast<size_t>(n) + 1, 0);
  std::vector<u64> indeg(static_cast<size_t>(n) + 1, 0);
  for (u32 i = 0; i < declared_m; ++i) {
    u32 u = sc.required(), v = sc.required(), w = sc.required();
    if (u >= n || v >= n) die("edge endpoint outside graph");
    if (u == v) continue;
    src.push_back(u);
    dst.push_back(v);
    wgt.push_back(w);
    ++outdeg[static_cast<size_t>(u) + 1];
    ++indeg[static_cast<size_t>(v) + 1];
  }

  const size_t m_edges = src.size();
  if (m_edges > std::numeric_limits<u32>::max()) die("too many edges for CSR");
  for (u32 i = 1; i <= n; ++i) {
    outdeg[i] += outdeg[i - 1];
    indeg[i] += indeg[i - 1];
  }
  fwd.off = outdeg;
  rev.off = indeg;
  fwd.edges.resize(m_edges);
  rev.edges.resize(m_edges);
  std::vector<u64> op = outdeg, ip = indeg;
  for (size_t i = 0; i < m_edges; ++i) {
    u32 u = src[i], v = dst[i], w = wgt[i];
    fwd.edges[static_cast<size_t>(op[u]++)] = {v, w};
    rev.edges[static_cast<size_t>(ip[v]++)] = {u, w};
  }
  src.clear(); src.shrink_to_fit();
  dst.clear(); dst.shrink_to_fit();
  wgt.clear(); wgt.shrink_to_fit();
}

struct HeapItem {
  u64 d;
  u32 v;
};
struct HeapCmp {
  bool operator()(const HeapItem &a, const HeapItem &b) const {
    return a.d > b.d;
  }
};

struct SearchSide {
  const CSR *g = nullptr;
  std::vector<u64> dist;
  std::vector<u32> stamp;
  u32 epoch = 0;
  std::priority_queue<HeapItem, std::vector<HeapItem>, HeapCmp> heap;

  explicit SearchSide(const CSR &graph) : g(&graph), dist(graph.n), stamp(graph.n, 0) {
    heap = decltype(heap)(HeapCmp{}, {});
  }
  void reset() {
    ++epoch;
    if (epoch == 0) {
      std::fill(stamp.begin(), stamp.end(), 0);
      epoch = 1;
    }
    heap = decltype(heap)(HeapCmp{}, {});
  }
  inline u64 get(u32 v) const {
    return stamp[v] == epoch ? dist[v] : INF;
  }
  inline void relax(u32 v, u64 d) {
    if (stamp[v] != epoch || d < dist[v]) {
      stamp[v] = epoch;
      dist[v] = d;
      heap.push({d, v});
    }
  }
  inline bool pop_valid(HeapItem &x) {
    while (!heap.empty()) {
      x = heap.top();
      heap.pop();
      if (stamp[x.v] == epoch && dist[x.v] == x.d) return true;
    }
    return false;
  }
  inline u64 min_valid() {
    while (!heap.empty()) {
      const HeapItem &x = heap.top();
      if (stamp[x.v] == epoch && dist[x.v] == x.d) return x.d;
      heap.pop();
    }
    return INF;
  }
};

struct Worker {
  SearchSide a, b;
  explicit Worker(const CSR &f, const CSR &r) : a(f), b(r) {}

  u64 solve(u32 s, u32 t) {
    if (s == t) return 0;
    a.reset();
    b.reset();
    a.relax(s, 0);
    b.relax(t, 0);
    u64 best = INF;
    HeapItem cur;

    while (true) {
      u64 da = a.min_valid(), db = b.min_valid();
      if (da == INF || db == INF || (best != INF && da >= best && db >= best))
        break;
      SearchSide &side = da <= db ? a : b;
      SearchSide &other = da <= db ? b : a;
      if (!side.pop_valid(cur)) break;
      const u64 du = cur.d;
      const u32 u = cur.v;
      const u64 meet = other.get(u);
      if (meet != INF && du <= INF - meet) best = std::min(best, du + meet);
      side.g->each(u, [&](const Edge &e) {
        if (du > INF - static_cast<u64>(e.weight)) return;
        const u64 nd = du + static_cast<u64>(e.weight);
        if (nd < side.get(e.to)) {
          side.relax(e.to, nd);
          const u64 od = other.get(e.to);
          if (od != INF && nd <= INF - od) best = std::min(best, nd + od);
        }
      });
      const u64 na = a.min_valid(), nb = b.min_valid();
      if (best != INF && na != INF && nb != INF && na >= best && nb >= best)
        break;
    }
    return best;
  }
};

static std::vector<int64_t> solve_all(const CSR &fwd, const CSR &rev,
                                      const std::vector<Query> &queries) {
  const size_t qn = queries.size();
  std::vector<int64_t> answer(qn, -1);
  std::vector<u32> order(qn);
  for (u32 i = 0; i < qn; ++i) order[i] = i;
  std::sort(order.begin(), order.end(), [&](u32 x, u32 y) {
    if (queries[x].s != queries[y].s) return queries[x].s < queries[y].s;
    return queries[x].t < queries[y].t;
  });
  struct Group { size_t begin, end; };
  std::vector<Group> groups;
  for (size_t i = 0; i < qn;) {
    size_t j = i + 1;
    const Query &q = queries[order[i]];
    while (j < qn && queries[order[j]].s == q.s && queries[order[j]].t == q.t) ++j;
    groups.push_back({i, j});
    i = j;
  }

  const unsigned hw = std::thread::hardware_concurrency();
  const unsigned workers = std::max(1u, std::min<unsigned>(12, hw ? hw : 1));
  std::atomic<size_t> next{0};
  std::vector<std::thread> pool;
  pool.reserve(workers);
  for (unsigned k = 0; k < workers; ++k) {
    pool.emplace_back([&]() {
      Worker worker(fwd, rev);
      for (;;) {
        size_t idx = next.fetch_add(1, std::memory_order_relaxed);
        if (idx >= groups.size()) break;
        const Group g = groups[idx];
        const Query &q = queries[order[g.begin]];
        const u64 d = worker.solve(q.s, q.t);
        const int64_t result = d == INF ? -1 :
            (d > static_cast<u64>(std::numeric_limits<int64_t>::max())
                 ? -1 : static_cast<int64_t>(d));
        for (size_t j = g.begin; j < g.end; ++j) answer[order[j]] = result;
      }
    });
  }
  for (auto &t : pool) t.join();
  return answer;
}

int main() {
  const char *base = std::getenv("SD_DATA");
  const char *out_env = std::getenv("SD_OUT");
  const std::string data = base ? base : "/data";
  const std::string out = out_env ? out_env : "/out";
  CSR forward, reverse;
  build_graph(data + "/graph.txt", forward, reverse);
  const std::vector<Query> queries = read_queries(data + "/queries.txt", forward.n);
  const std::vector<int64_t> answers = solve_all(forward, reverse, queries);

  const std::string path = out + "/distances.txt";
  FILE *file = std::fopen(path.c_str(), "wb");
  if (!file) die("cannot open output");
  char buf[32];
  for (int64_t value : answers) {
    auto r = std::to_chars(buf, buf + sizeof(buf), value);
    if (r.ec != std::errc()) die("integer formatting failed");
    if (std::fwrite(buf, 1, static_cast<size_t>(r.ptr - buf), file) !=
        static_cast<size_t>(r.ptr - buf) ||
        std::fputc('\n', file) == EOF) die("output write failed");
  }
  if (std::fclose(file)) die("output close failed");
  return 0;
}
