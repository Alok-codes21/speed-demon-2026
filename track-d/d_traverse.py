"""Track D (Traverse): optimized."""

import argparse
import os
from heapq import heappop, heappush


def main(data_dir, out_dir):
    # ---- fast graph load ----
    with open(os.path.join(data_dir, "graph.txt"), "rb") as f:
        tok = f.read().split()
    n, m = int(tok[0]), int(tok[1])
    adj = [[] for _ in range(n)]
    vals = list(map(int, tok[2:2 + 3 * m]))
    for u, v, w in zip(vals[0::3], vals[1::3], vals[2::3]):
        adj[u].append((v, w))
    del tok, vals

    # ---- load queries, group by source ----
    with open(os.path.join(data_dir, "queries.txt"), "rb") as f:
        q = list(map(int, f.read().split()))
    ss, ts = q[0::2], q[1::2]

    by_source = {}
    for s, t in zip(ss, ts):
        if s != t:
            by_source.setdefault(s, set()).add(t)

    # ---- one Dijkstra per unique source ----
    N = n  # heap entry = dist * N + node (one int instead of a tuple)
    INF = float("inf")
    dist = [INF] * n
    ans = {}

    for s, remaining in by_source.items():
        dist[s] = 0
        touched = [s]
        heap = [s]  # 0 * N + s
        while heap:
            x = heappop(heap)
            d = x // N
            u = x - d * N
            if d > dist[u]:
                continue  # stale entry
            if u in remaining:
                ans[(s, u)] = d
                remaining.discard(u)
                """Track D (Traverse): bidirectional Dijkstra."""

import argparse
import os
from heapq import heappop, heappush

INF = 1 << 62


def main(data_dir, out_dir):
    # ---- fast load: forward and reverse adjacency ----
    with open(os.path.join(data_dir, "graph.txt"), "rb") as f:
        tok = f.read().split()
    n, m = int(tok[0]), int(tok[1])
    adj = [[] for _ in range(n)]
    radj = [[] for _ in range(n)]
    vals = list(map(int, tok[2:2 + 3 * m]))
    for u, v, w in zip(vals[0::3], vals[1::3], vals[2::3]):
        adj[u].append((v, w))
        radj[v].append((u, w))
    del tok, vals

    with open(os.path.join(data_dir, "queries.txt"), "rb") as f:
        q = list(map(int, f.read().split()))

    cache = {}
    out = []
    for i in range(0, len(q), 2):
        s, t = q[i], q[i + 1]
        key = (s, t)
        r = cache.get(key)
        if r is None:
            r = cache[key] = bidir(adj, radj, s, t)
        out.append(r)

    os.makedirs(out_dir, exist_ok=True)
    with open(os.path.join(out_dir, "distances.txt"), "w", encoding="ascii", newline="\n") as f:
        f.write("".join(f"{d}\n" for d in out))


def bidir(adj, radj, s, t):
    if s == t:
        return 0
    df = {s: 0}   # forward distances from s
    db = {t: 0}   # backward distances to t
    hf = [(0, s)]
    hb = [(0, t)]
    mu = INF      # best s-t path length found so far

    while hf and hb:
        if hf[0][0] + hb[0][0] >= mu:
            break  # no better path can exist
        if len(hf) <= len(hb):
            d, u = heappop(hf)
            if d > df[u]:
                continue
            for v, w in adj[u]:
                nd = d + w
                if nd < df.get(v, INF):
                    df[v] = nd
                    heappush(hf, (nd, v))
                    x = db.get(v)
                    if x is not None and nd + x < mu:
                        mu = nd + x
        else:
            d, u = heappop(hb)
            if d > db[u]:
                continue
            for v, w in radj[u]:
                nd = d + w
                if nd < db.get(v, INF):
                    db[v] = nd
                    heappush(hb, (nd, v))
                    x = df.get(v)
                    if x is not None and nd + x < mu:
                        mu = nd + x

    return mu if mu < INF else -1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Track D (Traverse) bidirectional")
    parser.add_argument("--data", default="/data")
    parser.add_argument("--out", default="/out")
    args = parser.parse_args()
    main(args.data, args.out)