import os
import sys
from array import array
from heapq import heappop, heappush
from itertools import accumulate


def csr_py(U, V, W, n):
    cnt = [0] * (n + 1)
    for u in U:
        cnt[u + 1] += 1
    ptr = list(accumulate(cnt))
    pos = ptr[:]
    ix = array("i", bytes(4 * len(U)))
    wt = array("q", bytes(8 * len(U)))
    for u, v, w in zip(U, V, W):
        p = pos[u]
        pos[u] = p + 1
        ix[p] = v
        wt[p] = w
    return ptr, ix, wt


def load_py(path):
    f = open(path, "rb")
    n, m = map(int, f.readline().split())
    U, V, W = array("i"), array("i"), array("q")
    while True:
        chunk = f.readlines(1 << 20)
        if not chunk:
            break
        t = b" ".join(chunk).split()
        U.extend(map(int, t[0::3]))
        V.extend(map(int, t[1::3]))
        W.extend(map(int, t[2::3]))
    return n, csr_py(U, V, W, n), csr_py(V, U, W, n)


def csr_np(np, u, v, w, n):
    o = np.argsort(u, kind="stable")
    p = np.zeros(n + 1, np.int64)
    np.cumsum(np.bincount(u, minlength=n), out=p[1:])
    return p.tolist(), array("i", v[o].astype(np.int32).tobytes()), array("q", w[o].tobytes())


def load(path):
    try:
        import numpy as np
        g = np.fromstring(open(path, "rb").read(), dtype=np.int64, sep=" ")
        n, m = int(g[0]), int(g[1])
        e = g[2:2 + 3 * m].reshape(m, 3)
        return n, csr_np(np, e[:, 0], e[:, 1], e[:, 2], n), csr_np(np, e[:, 1], e[:, 0], e[:, 2], n)
    except Exception:
        return load_py(path)


def main(d, o):
    n, (fp, fv, fw), (rp, rv, rw) = load(d + "/graph.txt")
    q = open(d + "/queries.txt", "rb").read().split()
    I = 1 << 62
    df = [I] * n
    db = [I] * n
    res = []
    for k in range(0, len(q), 2):
        s, t = int(q[k]), int(q[k + 1])
        if s == t:
            res.append(0)
            continue
        df[s] = 0
        db[t] = 0
        hf, hb = [(0, s)], [(0, t)]
        tf, tb = [s], [t]
        mu = I
        while hf and hb and hf[0][0] + hb[0][0] < mu:
            if len(hf) <= len(hb):
                x, u = heappop(hf)
                if x > df[u]:
                    continue
                a, b = fp[u], fp[u + 1]
                for v, w in zip(fv[a:b], fw[a:b]):
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
                a, b = rp[u], rp[u + 1]
                for v, w in zip(rv[a:b], rw[a:b]):
                    y = x + w
                    if y < db[v]:
                        if db[v] == I:
                            tb.append(v)
                        db[v] = y
                        heappush(hb, (y, v))
                        mu = min(mu, y + df[v])
        res.append(mu if mu < I else -1)
        for v in tf:
            df[v] = I
        for v in tb:
            db[v] = I
    with open(o + "/distances.txt", "w", encoding="ascii", newline="\n") as f:
        f.write("".join(f"{x}\n" for x in res))


if __name__ == "__main__":
    data, out = "/data", "/out"
    a = sys.argv[1:]
    for i in range(0, len(a) - 1, 2):
        if a[i] == "--data":
            data = a[i + 1]
        elif a[i] == "--out":
            out = a[i + 1]
    os.makedirs(out, exist_ok=True)
    main(data, out)
