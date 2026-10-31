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
                    import os
import sys
from heapq import heappop, heappush


def main(d, o):
    t = open(d + "/graph.txt", "rb").read().split()
    n, m = int(t[0]), int(t[1])
    a = [[] for _ in range(n)]
    r = [[] for _ in range(n)]
    for i in range(2, 2 + 3 * m, 3):
        u, v, w = int(t[i]), int(t[i + 1]), int(t[i + 2])
        a[u].append((v, w))
        r[v].append((u, w))
    q = open(d + "/queries.txt", "rb").read().split()
    I = 1 << 62
    df = [I] * n
    db = [I] * n
    res = []
    for k in range(0, len(q), 2):
        s, e = int(q[k]), int(q[k + 1])
        if s == e:
            res.append(0)
            continue
        df[s] = 0
        db[e] = 0
        hf, hb = [(0, s)], [(0, e)]
        tf, tb = [s], [e]
        mu = I
        while hf and hb and hf[0][0] + hb[0][0] < mu:
            if len(hf) <= len(hb):
                x, u = heappop(hf)
                if x > df[u]:
                    continue
                for v, w in a[u]:
                    y = x + w
                    if y < df[v]:
                        if df[v] == I:
                            tf.append(v)
                        df[v] = y
                        heappush(hf, (y, v))
                        mu = min(mu, y + db[v])
            else:
                x, u = heappop(hb)
                if x > db[u]:
                    continue
                import os
import sys
from heapq import heappop, heappush


def main(d, o):
    t = open(d + "/graph.txt", "rb").read().split()
    n, m = int(t[0]), int(t[1])
    a = [[] for _ in range(n)]
    r = [[] for _ in range(n)]
    for i in range(2, 2 + 3 * m, 3):
        u, v, w = int(t[i]), int(t[i + 1]), int(t[i + 2])
        a[u].append((v, w))
        r[v].append((u, w))
    q = open(d + "/queries.txt", "rb").read().split()
    I = 1 << 62
    df = [I] * n
    db = [I] * n
    res = []
    for k in range(0, len(q), 2):
        s, e = int(q[k]), int(q[k + 1])
        if s == e:
            res.append(0)
            continue
        df[s] = 0
        db[e] = 0
        hf, hb = [(0, s)], [(0, e)]
        tf, tb = [s], [e]
        mu = I
        while hf and hb and hf[0][0] + hb[0][0] < mu:
            if len(hf) <= len(hb):
                x, u = heappop(hf)
                if x > df[u]:
                    continue
                for v, w in a[u]:
                    y = x + w
                    if y < df[v]:
                        if df[v] == I:
                            tf.append(v)
                        df[v] = y
                        heappush(hf, (y, v))
                        mu = min(mu, y + db[v])
            else:
                x, u = heappop(hb)
                if x > db[u]:
                    continue
                
import os
import sys
from array import array
from heapq import heappop, heappush


INF = 1 << 62


def csr_py(U, V, W, n):
    cnt = [0] * (n + 1)

    for u in U:
        cnt[u + 1] += 1

    ptr = [0] * (n + 1)

    for i in range(n):
        ptr[i + 1] = ptr[i] + cnt[i]

    pos = ptr[:]
    ix = array("i", [0]) * len(U)
    wt = array("q", [0]) * len(U)

    for u, v, w in zip(U, V, W):
        p = pos[u]
        pos[u] += 1
        ix[p] = v
        wt[p] = w

    return ptr, ix, wt


def load_py(path):
    with open(path, "rb") as f:
        n, m = map(int, f.readline().split())

        U, V, W = array("i"), array("i"), array("q")

        while True:
            chunk = f.readlines(1 << 20)
            if not chunk:
                break

            tokens = b" ".join(chunk).split()

            U.extend(map(int, tokens[0::3]))
            V.extend(map(int, tokens[1::3]))
            W.extend(map(int, tokens[2::3]))

    if len(U) != m:
        raise ValueError("Invalid graph edge count")

    return (
        n,
        csr_py(U, V, W, n),
        csr_py(V, U, W, n)
    )


def csr_np(np, u, v, w, n):
    order = np.argsort(u)

    counts = np.bincount(u, minlength=n)

    ptr = np.empty(n + 1, dtype=np.int64)
    ptr[0] = 0
    np.cumsum(counts, out=ptr[1:])

    ix = v[order].astype(np.int32, copy=False)
    wt = w[order].astype(np.int64, copy=False)

    return ptr, ix, wt


def load(path):
    try:
        import numpy as np

        g = np.fromfile(path, dtype=np.int64, sep=" ")

        n, m = int(g[0]), int(g[1])

        if g.size != 2 + 3 * m:
            raise ValueError("Invalid graph file")

        e = g[2:].reshape(m, 3)

        forward = csr_np(
            np, e[:, 0], e[:, 1], e[:, 2], n
        )

        reverse = csr_np(
            np, e[:, 1], e[:, 0], e[:, 2], n
        )

        return n, forward, reverse

    except ImportError:
        return load_py(path)


def bidirectional_dijkstra(s, t, fp, fv, fw, rp, rv, rw, df, db):

    if s == t:
        return 0

    df[s] = 0
    db[t] = 0

    hf = [(0, s)]
    hb = [(0, t)]

    tf = [s]
    tb = [t]

    mu = INF

    while hf and hb:

        # Remove stale forward heap entries
        while hf and hf[0][0] != df[hf[0][1]]:
            heappop(hf)

        # Remove stale backward heap entries
        while hb and hb[0][0] != db[hb[0][1]]:
            heappop(hb)

        if not hf or not hb:
            break

        # Optimal stopping condition
        if hf[0][0] + hb[0][0] >= mu:
            break

        # Expand the side with the smaller minimum distance
        if hf[0][0] <= hb[0][0]:

            x, u = heappop(hf)

            if db[u] != INF:
                mu = min(mu, x + db[u])

            a = fp[u]
            b = fp[u + 1]

            for j in range(a, b):
                v = fv[j]
                y = x + fw[j]

                if y < df[v]:

                    if df[v] == INF:
                        tf.append(v)

                    df[v] = y
                    heappush(hf, (y, v))

                    if db[v] != INF:
                        mu = min(mu, y + db[v])

        else:

            x, u = heappop(hb)

            if df[u] != INF:
                mu = min(mu, x + df[u])

            a = rp[u]
            b = rp[u + 1]

            for j in range(a, b):
                v = rv[j]
                y = x + rw[j]

                if y < db[v]:

                    if db[v] == INF:
                        tb.append(v)

                    db[v] = y
                    heappush(hb, (y, v))

                    if df[v] != INF:
                        mu = min(mu, y + df[v])

    # Reset only vertices touched by this query
    for v in tf:
        df[v] = INF

    for v in tb:
        db[v] = INF

    return mu if mu < INF else -1


def main(data_dir, out_dir):

    n, forward, reverse = load(
        os.path.join(data_dir, "graph.txt")
    )

    fp, fv, fw = forward
    rp, rv, rw = reverse

    with open(
        os.path.join(data_dir, "queries.txt"), "rb"
    ) as f:
        q = list(map(int, f.read().split()))

    if len(q) % 2:
        raise ValueError("Invalid query count")

    df = [INF] * n
    db = [INF] * n

    output_path = os.path.join(out_dir, "distances.txt")

    with open(output_path, "w", encoding="ascii") as out:

        for k in range(0, len(q), 2):

            s = q[k]
            t = q[k + 1]

            if not (0 <= s < n and 0 <= t < n):
                raise ValueError("Invalid source or destination")

            ans = bidirectional_dijkstra(
                s, t,
                fp, fv, fw,
                rp, rv, rw,
                df, db
            )

            out.write(str(ans) + "\n")


if __name__ == "__main__":

    data = "/data"
    output = "/out"

    args = sys.argv[1:]

    for i in range(0, len(args) - 1, 2):

        if args[i] == "--data":
            data = args[i + 1]

        elif args[i] == "--out":
            output = args[i + 1]

    os.makedirs(output, exist_ok=True)

    main(data, output)
