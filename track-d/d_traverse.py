#!/usr/bin/env python3
"""Track D shortest paths: Python 3.13 standard library implementation.

Uses compact forward/reverse CSR arrays, memory-mapped two-pass graph parsing,
bidirectional Dijkstra, duplicate caching, and touched-node distance resets.
"""
import os
import sys
from array import array
from heapq import heappop, heappush

# ─── sentinel ────────────────────────────────────────────────────────────────
INF = 1 << 62


# ══════════════════════════════════════════════════════════════════════════════
# §1  CSR construction
# ══════════════════════════════════════════════════════════════════════════════

def _load_fast(path):
    import mmap
    with open(path, "rb") as f:
        data = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
        size = len(data)
        i = 0
        def number():
            nonlocal i
            while i < size and data[i] <= 32:
                i += 1
            j = i
            while j < size and data[j] > 32:
                j += 1
            x = int(data[i:j])
            i = j
            return x
        n, m = number(), number()
        outdeg, indeg = array("I", [0]) * n, array("I", [0]) * n
        for _ in range(m):
            u, v, w = number(), number(), number()
            if u != v:
                outdeg[u] += 1
                indeg[v] += 1
        fp, rp = array("I", [0]) * (n + 1), array("I", [0]) * (n + 1)
        for v in range(n):
            fp[v + 1] = fp[v] + outdeg[v]
            rp[v + 1] = rp[v] + indeg[v]
        del outdeg, indeg
        fc, fw = array("I", [0]) * fp[n], array("I", [0]) * fp[n]
        rc, rw = array("I", [0]) * rp[n], array("I", [0]) * rp[n]
        i = 0
        number(); number()
        for _ in range(m):
            u, v, w = number(), number(), number()
            if u == v:
                continue
            x = fp[u]
            fc[x], fw[x] = v, w
            fp[u] = x + 1
            x = rp[v]
            rc[x], rw[x] = u, w
            rp[v] = x + 1
        for v in range(n, 0, -1):
            fp[v], rp[v] = fp[v - 1], rp[v - 1]
        fp[0] = rp[0] = 0
        data.close()
    return n, (fp, fc, fw), (rp, rc, rw)

load = _load_fast


# ══════════════════════════════════════════════════════════════════════════════
# §3  Bidirectional Dijkstra  (optimised)
# ══════════════════════════════════════════════════════════════════════════════

def _run_queries(q_raw, n, fp, fc, fw, rp, rc, rw):
    """
    Process all queries and return a list of distances.

    Hot-path variables bound to locals before the outer loop to avoid
    LOAD_GLOBAL and LOAD_ATTR overhead on every iteration.

    Heap packing:  heap entry = dist * n + node   (Python ints: no overflow).
    Termination:   break when  min_key_fwd + min_key_bwd >= mu
                   (min key = heap[0] // n).
    Direction:     always expand from the heap with the SMALLER min-key;
                   this is the Kaindl-Kainz criterion and is strictly
                   better than the "smaller heap" heuristic in the reference.
    """
    I    = INF
    _pop = heappop
    _psh = heappush

    # persistent distance arrays – reset per query via touched-node lists
    df = array('Q', [I]) * n
    db = array('Q', [I]) * n

    res = [None] * (len(q_raw) // 2)
    answer_cache = {}

    for k in range(0, len(q_raw), 2):
        out_idx = k // 2
        s, t = int(q_raw[k]), int(q_raw[k + 1])

        if s == t:
            res[out_idx] = 0
            continue

        # The input intentionally contains duplicate queries.
        key = (s, t)
        cached = answer_cache.get(key)
        if cached is not None:
            res[out_idx] = cached
            continue

        # A distinct target is unreachable from a zero-outdegree source.
        # Likewise, a zero-indegree target cannot be reached from elsewhere.
        if fp[s] == fp[s + 1] or rp[t] == rp[t + 1]:
            answer_cache[key] = -1
            res[out_idx] = -1
            continue

        # A very low-outdegree source (notably the injected heavy-edge
        # sources) is cheaper to search forward from than to grow a large
        # reverse frontier from the target.
        if fp[s + 1] - fp[s] <= 2:
            df[s] = 0
            touched = [s]
            heap = [s]
            answer = -1
            while heap:
                entry = _pop(heap)
                x = entry // n
                u = entry - x * n
                if x != df[u]:
                    continue
                if u == t:
                    answer = x
                    break
                for j in range(fp[u], fp[u + 1]):
                    v = fc[j]
                    y = x + fw[j]
                    if y < df[v]:
                        if df[v] == I:
                            touched.append(v)
                        df[v] = y
                        _psh(heap, y * n + v)
            answer_cache[key] = answer
            res[out_idx] = answer
            for v in touched:
                df[v] = I
            continue

        # If the target has very few incoming edges, reverse Dijkstra
        # is cheaper than maintaining two frontiers. This covers the
        # benchmark's injected sink queries and other low-indegree targets.
        if rp[t + 1] - rp[t] <= 2:
            db[t] = 0
            touched = [t]
            heap = [t]
            answer = -1
            while heap:
                entry = _pop(heap)
                x = entry // n
                u = entry - x * n
                if x != db[u]:
                    continue
                if u == s:
                    answer = x
                    break
                for j in range(rp[u], rp[u + 1]):
                    v = rc[j]
                    y = x + rw[j]
                    if y < db[v]:
                        if db[v] == I:
                            touched.append(v)
                        db[v] = y
                        _psh(heap, y * n + v)
            answer_cache[key] = answer
            res[out_idx] = answer
            for v in touched:
                db[v] = I
            continue

        # ── initialise ───────────────────────────────────────────────────────
        df[s] = 0;  db[t] = 0
        hf = [s];   hb = [t]      # packed: 0*n+s, 0*n+t
        tf = [s];   tb = [t]      # touched nodes for reset
        mu = I

        # ── bidirectional Dijkstra ───────────────────────────────────────────
        while hf and hb:
            # Remove stale heap heads before evaluating the stopping bound.
            # This keeps the bound tight and avoids expanding from an obsolete
            # (artificially small) frontier distance.
            while hf:
                top = hf[0]
                top_d = top // n
                if top_d == df[top - top_d * n]:
                    break
                _pop(hf)
            while hb:
                top = hb[0]
                top_d = top // n
                if top_d == db[top - top_d * n]:
                    break
                _pop(hb)
            if not hf or not hb:
                break

            hf_d = hf[0] // n
            hb_d = hb[0] // n
            if hf_d + hb_d >= mu:
                break

            if hf_d <= hb_d:
                # ── forward step ─────────────────────────────────────────────
                entry = _pop(hf)
                x = entry // n
                u = entry - x * n
                a = fp[u];  b = fp[u + 1]
                for j in range(a, b):
                    v = fc[j];  y = x + fw[j]
                    if y < df[v]:
                        if df[v] == I: tf.append(v)
                        df[v] = y
                        _psh(hf, y * n + v)
                        c = y + db[v]
                        if c < mu: mu = c
            else:
                # ── backward step ─────────────────────────────────────────────
                entry = _pop(hb)
                x = entry // n
                u = entry - x * n
                a = rp[u];  b = rp[u + 1]
                for j in range(a, b):
                    v = rc[j];  y = x + rw[j]
                    if y < db[v]:
                        if db[v] == I: tb.append(v)
                        db[v] = y
                        _psh(hb, y * n + v)
                        c = y + df[v]
                        if c < mu: mu = c

        answer = mu if mu < I else -1
        answer_cache[key] = answer
        res[out_idx] = answer

        # ── cheap reset: only touch nodes we actually visited ─────────────────
        for v in tf: df[v] = I
        for v in tb: db[v] = I

    return res


# ══════════════════════════════════════════════════════════════════════════════
# §4  Entry point
# ══════════════════════════════════════════════════════════════════════════════

def main(d, o):
    n, (fp, fc, fw), (rp, rc, rw) = load(d + "/graph.txt")
    q_raw = open(d + "/queries.txt", "rb").read().split()

    res = _run_queries(q_raw, n, fp, fc, fw, rp, rc, rw)

    os.makedirs(o, exist_ok=True)
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
