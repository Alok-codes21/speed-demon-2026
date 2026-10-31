
import os
import sys
from array import array
from heapq import heappop, heappush


INF = 1 << 62


def load_graph(path):
    import numpy as np

    g = np.fromfile(path, dtype=np.int64, sep=" ")

    n, m = int(g[0]), int(g[1])

    if g.size != 2 + 3 * m:
        raise ValueError("Invalid graph file")

    edges = g[2:].reshape(m, 3)

    u = edges[:, 0]
    v = edges[:, 1]
    w = edges[:, 2]

    if (u < 0).any() or (u >= n).any():
        raise ValueError("Invalid edge source")

    if (v < 0).any() or (v >= n).any():
        raise ValueError("Invalid edge destination")

    if (w < 0).any():
        raise ValueError("Dijkstra requires non-negative weights")

    order = np.argsort(u)

    counts = np.bincount(u, minlength=n)

    ptr = np.empty(n + 1, dtype=np.int64)
    ptr[0] = 0
    np.cumsum(counts, out=ptr[1:])

    sorted_v = v[order].astype(np.int32, copy=False)
    sorted_w = w[order].astype(np.int64, copy=False)

    fv = array("i")
    fw = array("q")

    fv.frombytes(sorted_v.tobytes())
    fw.frombytes(sorted_w.tobytes())

    del g, edges, u, v, w, order
    del sorted_v, sorted_w, counts

    return n, ptr.tolist(), fv, fw


def solve(n, ptr, fv, fw, queries):

    answers = [-1] * (len(queries) // 2)

    # Group destinations by source.
    groups = {}

    for i in range(0, len(queries), 2):
        s, t = queries[i], queries[i + 1]

        if not (0 <= s < n and 0 <= t < n):
            raise ValueError("Invalid query")

        if s == t:
            answers[i // 2] = 0
            continue

        if s not in groups:
            groups[s] = {}

        groups[s].setdefault(t, []).append(i // 2)

    dist = [INF] * n

    # Process each unique source once.
    for s, pending in groups.items():

        dist[s] = 0
        touched = [s]

        # Packed heap entry: distance * n + vertex
        heap = [s]

        remaining = len(pending)

        while heap and remaining:

            entry = heappop(heap)

            d = entry // n
            u = entry - d * n

            if d != dist[u]:
                continue

            # Destination reached with its shortest distance.
            indices = pending.pop(u, None)

            if indices is not None:
                for idx in indices:
                    answers[idx] = d

                remaining -= 1

                if remaining == 0:
                    break

            # Traverse CSR adjacency.
            for j in range(ptr[u], ptr[u + 1]):

                v = fv[j]
                nd = d + fw[j]

                if nd < dist[v]:

                    if dist[v] == INF:
                        touched.append(v)

                    dist[v] = nd

                    heappush(heap, nd * n + v)

        # Reset only visited vertices.
        for v in touched:
            dist[v] = INF

    return answers


def main(data_dir, out_dir):

    graph_path = os.path.join(data_dir, "graph.txt")
    query_path = os.path.join(data_dir, "queries.txt")

    n, ptr, fv, fw = load_graph(graph_path)

    with open(query_path, "rb") as f:
        queries = list(map(int, f.read().split()))

    if len(queries) % 2:
        raise ValueError("Invalid query file")

    answers = solve(n, ptr, fv, fw, queries)

    os.makedirs(out_dir, exist_ok=True)

    output_path = os.path.join(out_dir, "distances.txt")

    with open(output_path, "w", encoding="ascii") as f:
        f.write("\n".join(map(str, answers)) + "\n")


if __name__ == "__main__":

    data = "/data"
    output = "/out"

    args = sys.argv[1:]

    for i in range(0, len(args) - 1, 2):

        if args[i] == "--data":
            data = args[i + 1]

        elif args[i] == "--out":
            output = args[i + 1]

    main(data, output)
