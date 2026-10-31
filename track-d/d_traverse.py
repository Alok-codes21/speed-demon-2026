import os
import sys
from array import array
from heapq import heappop, heappush
from itertools import accumulate
import shutil
import subprocess
import tempfile


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


C_SRC = r'''#include <stdio.h>
#include <stdlib.h>
typedef long long ll;
typedef struct { ll d; int v; } E;
typedef struct { E *a; size_t n, c; } H;
static char *buf; static size_t len, pos;
static void slurp(const char *p) {
    FILE *f = fopen(p, "rb");
    if (!f) exit(2);
    fseek(f, 0, SEEK_END); len = ftell(f); rewind(f);
    buf = malloc(len + 2);
    if (fread(buf, 1, len, f) != len) exit(2);
    buf[len] = 0; fclose(f);
}
static inline ll rd(void) {
    while (buf[pos] && buf[pos] < '0') pos++;
    ll x = 0;
    while (buf[pos] >= '0') x = x * 10 + buf[pos++] - '0';
    return x;
}
static void push(H *h, ll d, int v) {
    if (h->n == h->c) { h->c = h->c ? h->c * 2 : 1024; h->a = realloc(h->a, h->c * sizeof(E)); }
    size_t i = h->n++;
    while (i) { size_t p = (i - 1) >> 1; if (h->a[p].d <= d) break; h->a[i] = h->a[p]; i = p; }
    h->a[i].d = d; h->a[i].v = v;
}
static E pop(H *h) {
    E t = h->a[0], l = h->a[--h->n];
    size_t i = 0, n = h->n;
    while (1) {
        size_t c = 2 * i + 1;
        if (c >= n) break;
        if (c + 1 < n && h->a[c + 1].d < h->a[c].d) c++;
        if (h->a[c].d >= l.d) break;
        h->a[i] = h->a[c]; i = c;
    }
    if (n) h->a[i] = l;
    return t;
}
int main(int argc, char **argv) {
    const char *d = argc > 1 ? argv[1] : "/data", *o = argc > 2 ? argv[2] : "/out";
    char path[4096];
    snprintf(path, sizeof path, "%s/graph.txt", d);
    slurp(path);
    int n = (int)rd(); long m = (long)rd();
    size_t start = pos;
    int *fp = calloc(n + 2, sizeof(int)), *rp = calloc(n + 2, sizeof(int));
    for (long i = 0; i < m; i++) { int u = (int)rd(), v = (int)rd(); rd(); fp[u + 1]++; rp[v + 1]++; }
    for (int i = 0; i < n; i++) { fp[i + 1] += fp[i]; rp[i + 1] += rp[i]; }
    int *fi = malloc(m * 4 + 4), *ri = malloc(m * 4 + 4);
    ll *fw = malloc(m * 8 + 8), *rw = malloc(m * 8 + 8);
    int *fq = malloc((n + 1) * 4), *rq = malloc((n + 1) * 4);
    for (int i = 0; i <= n; i++) { fq[i] = fp[i]; rq[i] = rp[i]; }
    pos = start;
    for (long i = 0; i < m; i++) {
        int u = (int)rd(), v = (int)rd(); ll w = rd();
        int a = fq[u]++; fi[a] = v; fw[a] = w;
        int b = rq[v]++; ri[b] = u; rw[b] = w;
    }
    free(buf); free(fq); free(rq);
    snprintf(path, sizeof path, "%s/queries.txt", d);
    slurp(path); pos = 0;
    const ll INF = 1LL << 61;
    ll *df = malloc(n * 8 + 8), *db = malloc(n * 8 + 8);
    for (int i = 0; i < n; i++) df[i] = db[i] = INF;
    int *tf = malloc(n * 4 + 4), *tb = malloc(n * 4 + 4);
    H hf = {0, 0, 0}, hb = {0, 0, 0};
    snprintf(path, sizeof path, "%s/distances.txt", o);
    FILE *out = fopen(path, "wb");
    if (!out) return 3;
    while (1) {
        while (pos < len && buf[pos] < '0') pos++;
        if (pos >= len) break;
        int s = (int)rd(), t = (int)rd();
        if (s == t) { fputs("0\n", out); continue; }
        hf.n = hb.n = 0;
        int nf = 0, nb = 0;
        df[s] = 0; db[t] = 0; tf[nf++] = s; tb[nb++] = t;
        push(&hf, 0, s); push(&hb, 0, t);
        ll mu = INF;
        while (hf.n && hb.n && hf.a[0].d + hb.a[0].d < mu) {
            if (hf.n <= hb.n) {
                E e = pop(&hf); int u = e.v; ll x = e.d;
                if (x > df[u]) continue;
                for (int k = fp[u]; k < fp[u + 1]; k++) {
                    int w = fi[k]; ll y = x + fw[k];
                    if (y < df[w]) {
                        if (df[w] == INF) tf[nf++] = w;
                        df[w] = y; push(&hf, y, w);
                        ll z = y + db[w]; if (z < mu) mu = z;
                    }
                }
            } else {
                E e = pop(&hb); int u = e.v; ll x = e.d;
                if (x > db[u]) continue;
                for (int k = rp[u]; k < rp[u + 1]; k++) {
                    int w = ri[k]; ll y = x + rw[k];
                    if (y < db[w]) {
                        if (db[w] == INF) tb[nb++] = w;
                        db[w] = y; push(&hb, y, w);
                        ll z = y + df[w]; if (z < mu) mu = z;
                    }
                }
            }
        }
        if (mu < INF) fprintf(out, "%lld\n", mu); else fputs("-1\n", out);
        for (int i = 0; i < nf; i++) df[tf[i]] = INF;
        for (int i = 0; i < nb; i++) db[tb[i]] = INF;
    }
    fclose(out);
    return 0;
}
'''


def run_c(d, o):
    cc = shutil.which("gcc") or shutil.which("cc") or shutil.which("clang")
    if not cc:
        return False
    try:
        with tempfile.TemporaryDirectory() as t:
            src, exe = t + "/s.c", t + "/s" + (".exe" if os.name == "nt" else "")
            with open(src, "w") as f:
                f.write(C_SRC)
            subprocess.run([cc, "-O2", "-o", exe, src], check=True, capture_output=True)
            subprocess.run([exe, d, o], check=True)
        return True
    except Exception:
        return False


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
    if not run_c(data, out):
        main(data, out)
