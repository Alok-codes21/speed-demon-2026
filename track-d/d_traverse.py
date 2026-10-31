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
                for v, w in r[u]:
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
    args = sys.argv[1:]
    for i in range(0, len(args) - 1, 2):
        if args[i] == "--data":
            data = args[i + 1]
        elif args[i] == "--out":
            out = args[i + 1]
    os.makedirs(out, exist_ok=True)
    main(data, out)
