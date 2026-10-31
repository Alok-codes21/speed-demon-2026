// Speed Demon 2026, Track B (Index). Linux, C++20, zero dependencies.
#include <algorithm>
#include <cstdint>
#include <cstring>
#include <fcntl.h>
#include <pthread.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#include <malloc.h>
#include <vector>
#include <string>

static uint8_t char_map[256];
static void init_map() {
    for (int i = 0; i < 256; ++i)
        char_map[i] = ((i >= 'a' && i <= 'z') || (i >= '0' && i <= '9')) ? (uint8_t)i :
                      (i >= 'A' && i <= 'Z') ? (uint8_t)(i + 32) : 0;
}

static inline uint64_t hash_term(const char* s, uint32_t len) {
    uint64_t h = 0xcbf29ce484222325ULL;
    for (uint32_t i = 0; i < len; ++i) { h ^= (uint8_t)s[i]; h *= 0x100000001b3ULL; }
    return h ? h : 1;
}

static inline uint64_t hash_corpus_token(const char* s, uint32_t len) {
    uint64_t h = 0xcbf29ce484222325ULL;
    for (uint32_t i = 0; i < len; ++i) { h ^= char_map[(uint8_t)s[i]]; h *= 0x100000001b3ULL; }
    return h ? h : 1;
}

const uint32_t VOCAB_CAP = 131072, VOCAB_MASK = VOCAB_CAP - 1;
struct VocabSlot { uint64_t hash; uint32_t term_id, len; char str[32]; };
static VocabSlot vocab[VOCAB_CAP];

struct TermInfo { uint32_t len; char str[32]; };
static TermInfo terms_info[VOCAB_CAP];
static uint32_t num_terms = 0;

static inline uint32_t intern_term(const char* s, uint32_t len) {
    if (len > 31) len = 31;
    uint64_t h = hash_term(s, len);
    uint32_t idx = h & VOCAB_MASK;
    while (vocab[idx].term_id) {
        if (vocab[idx].hash == h && vocab[idx].len == len && memcmp(vocab[idx].str, s, len) == 0)
            return vocab[idx].term_id - 1;
        idx = (idx + 1) & VOCAB_MASK;
    }
    uint32_t id = num_terms++;
    vocab[idx] = {h, id + 1, len, {}};
    memcpy(vocab[idx].str, s, len);
    terms_info[id] = {len, {}};
    memcpy(terms_info[id].str, s, len);
    return id;
}

static inline int lookup_corpus_token(const char* s, uint32_t len) {
    if (len > 31) len = 31;
    uint64_t h = hash_corpus_token(s, len);
    uint32_t idx = h & VOCAB_MASK;
    while (vocab[idx].term_id) {
        if (vocab[idx].hash == h && vocab[idx].len == len) {
            bool eq = true;
            for (uint32_t i = 0; i < len; ++i) {
                if (char_map[(uint8_t)s[i]] != (uint8_t)vocab[idx].str[i]) { eq = false; break; }
            }
            if (eq) return (int)(vocab[idx].term_id - 1);
        }
        idx = (idx + 1) & VOCAB_MASK;
    }
    return -1;
}

enum OpType : uint8_t { OP_AND, OP_OR, OP_NOT };
struct Query {
    uint8_t is_phrase, n_terms;
    uint32_t terms[8];
    OpType ops[7];
};
static std::vector<Query> queries;

void parse_queries(const std::string& path) {
    int fd = open(path.c_str(), O_RDONLY);
    if (fd < 0) return;
    struct stat st; fstat(fd, &st);
    const char* data = (const char*)mmap(nullptr, st.st_size, PROT_READ, MAP_SHARED, fd, 0);
    close(fd);
    const char *p = data, *end = data + st.st_size;
    while (p < end) {
        if (*p == '\r' || *p == '\n') { p++; continue; }
        Query q{};
        if (p + 5 < end && memcmp(p, "BOOL ", 5) == 0) {
            p += 5;
            uint32_t tc = 0;
            while (p < end && *p != '\r' && *p != '\n') {
                const char* s = p;
                while (p < end && *p != ' ' && *p != '\r' && *p != '\n') p++;
                q.terms[tc++] = intern_term(s, p - s);
                if (p >= end || *p == '\r' || *p == '\n') break;
                s = ++p;
                while (p < end && *p != ' ' && *p != '\r' && *p != '\n') p++;
                q.ops[tc - 1] = (p - s == 3 && memcmp(s, "AND", 3) == 0) ? OP_AND :
                                (p - s == 2 && memcmp(s, "OR", 2) == 0)  ? OP_OR  : OP_NOT;
                if (p < end && *p == ' ') p++;
            }
            q.n_terms = tc;
        } else if (p + 8 < end && memcmp(p, "PHRASE \"", 8) == 0) {
            q.is_phrase = 1; p += 8;
            uint32_t tc = 0;
            while (p < end && *p != '"') {
                const char* s = p;
                while (p < end && *p != ' ' && *p != '"') p++;
                q.terms[tc++] = intern_term(s, p - s);
                if (p < end && *p == ' ') p++;
            }
            if (p < end && *p == '"') p++;
            q.n_terms = tc;
        }
        queries.push_back(q);
        while (p < end && *p != '\n') p++;
        if (p < end && *p == '\n') p++;
    }
    munmap((void*)data, st.st_size);
}

struct DocSpan { uint32_t off, len; };
static DocSpan doc_spans[262144];
static uint32_t* global_postings = nullptr;
static uint32_t postings_offsets[VOCAB_CAP + 1];
static const char* global_corpus = nullptr;

const int N_THREADS = 12;
struct WorkerState {
    const char *start = nullptr, *end = nullptr;
    std::vector<uint32_t> counts, last_doc;
    std::vector<uint32_t*> write_ptrs;
    size_t q_s = 0, q_e = 0;
    std::string out;
};
static WorkerState workers[N_THREADS];

static void* scan_pass1(void* arg) {
    WorkerState* w = (WorkerState*)arg;
    const char *p = w->start, *end = w->end;
    std::fill(w->last_doc.begin(), w->last_doc.end(), 0);
    while (p < end) {
        uint32_t doc_id = 0;
        while (*p >= '0' && *p <= '9') doc_id = doc_id * 10 + (*p++ - '0');
        if (*p == '\t') p++;
        const char* tstart = p;
        while (p < end && *p != '\n') {
            while (p < end && *p != '\n' && !char_map[(uint8_t)*p]) p++;
            if (p >= end || *p == '\n') break;
            const char* tok_s = p;
            while (p < end && char_map[(uint8_t)*p]) p++;
            uint32_t tok_len = (uint32_t)(p - tok_s);
            int tid = lookup_corpus_token(tok_s, tok_len);
            if (tid >= 0 && w->last_doc[tid] != doc_id + 1) {
                w->last_doc[tid] = doc_id + 1;
                w->counts[tid]++;
            }
        }
        doc_spans[doc_id] = {(uint32_t)(tstart - global_corpus), (uint32_t)(p - tstart)};
        if (p < end && *p == '\n') p++;
    }
    return nullptr;
}

static void* scan_pass2(void* arg) {
    WorkerState* w = (WorkerState*)arg;
    const char *p = w->start, *end = w->end;
    std::fill(w->last_doc.begin(), w->last_doc.end(), 0);
    while (p < end) {
        uint32_t doc_id = 0;
        while (*p >= '0' && *p <= '9') doc_id = doc_id * 10 + (*p++ - '0');
        if (*p == '\t') p++;
        while (p < end && *p != '\n') {
            while (p < end && *p != '\n' && !char_map[(uint8_t)*p]) p++;
            if (p >= end || *p == '\n') break;
            const char* tok_s = p;
            while (p < end && char_map[(uint8_t)*p]) p++;
            uint32_t tok_len = (uint32_t)(p - tok_s);
            int tid = lookup_corpus_token(tok_s, tok_len);
            if (tid >= 0 && w->last_doc[tid] != doc_id + 1) {
                w->last_doc[tid] = doc_id + 1;
                *w->write_ptrs[tid]++ = doc_id;
            }
        }
        if (p < end && *p == '\n') p++;
    }
    return nullptr;
}

static inline bool bsearch_doc(const uint32_t* arr, uint32_t n, uint32_t x) {
    uint32_t l = 0, r = n;
    while (l < r) {
        uint32_t mid = (l + r) >> 1;
        if (arr[mid] < x) l = mid + 1;
        else r = mid;
    }
    return l < n && arr[l] == x;
}

static inline void intersect(const uint32_t* a, uint32_t na, const uint32_t* b, uint32_t nb, std::vector<uint32_t>& out) {
    out.clear();
    if (!na || !nb) return;
    if (nb >= 16 * na) {
        out.reserve(na);
        for (uint32_t i = 0; i < na; ++i) if (bsearch_doc(b, nb, a[i])) out.push_back(a[i]);
        return;
    }
    if (na >= 16 * nb) {
        out.reserve(nb);
        for (uint32_t j = 0; j < nb; ++j) if (bsearch_doc(a, na, b[j])) out.push_back(b[j]);
        return;
    }
    out.reserve(std::min(na, nb));
    uint32_t i = 0, j = 0;
    while (i < na && j < nb) {
        if (a[i] == b[j]) { out.push_back(a[i]); i++; j++; }
        else if (a[i] < b[j]) i++;
        else j++;
    }
}

static inline void union_op(const uint32_t* a, uint32_t na, const uint32_t* b, uint32_t nb, std::vector<uint32_t>& out) {
    out.clear(); out.reserve(na + nb);
    uint32_t i = 0, j = 0;
    while (i < na && j < nb) {
        if (a[i] == b[j]) { out.push_back(a[i]); i++; j++; }
        else if (a[i] < b[j]) out.push_back(a[i++]);
        else out.push_back(b[j++]);
    }
    while (i < na) out.push_back(a[i++]);
    while (j < nb) out.push_back(b[j++]);
}

static inline void diff_op(const uint32_t* a, uint32_t na, const uint32_t* b, uint32_t nb, std::vector<uint32_t>& out) {
    out.clear();
    if (!na) return;
    if (!nb) { out.assign(a, a + na); return; }
    if (nb >= 16 * na) {
        out.reserve(na);
        for (uint32_t i = 0; i < na; ++i) if (!bsearch_doc(b, nb, a[i])) out.push_back(a[i]);
        return;
    }
    out.reserve(na);
    uint32_t i = 0, j = 0;
    while (i < na && j < nb) {
        if (a[i] == b[j]) { i++; j++; }
        else if (a[i] < b[j]) out.push_back(a[i++]);
        else j++;
    }
    while (i < na) out.push_back(a[i++]);
}

struct PhraseWord { uint32_t len; const char* str; };

bool verify_phrase_fast(uint32_t doc_id, const PhraseWord* pw, uint32_t k) {
    const char* p = global_corpus + doc_spans[doc_id].off;
    const char* end = p + doc_spans[doc_id].len;
    uint32_t matched = 0;
    while (p < end) {
        while (p < end && !char_map[(uint8_t)*p]) p++;
        if (p >= end) break;
        const char* tstart = p;
        while (p < end && char_map[(uint8_t)*p]) p++;
        uint32_t tlen = (uint32_t)(p - tstart);

        bool eq = (tlen == pw[matched].len);
        if (eq) {
            for (uint32_t i = 0; i < tlen; ++i) {
                if (char_map[(uint8_t)tstart[i]] != (uint8_t)pw[matched].str[i]) {
                    eq = false; break;
                }
            }
        }
        if (eq) {
            matched++;
            if (matched == k) return true;
        } else if (matched > 0) {
            bool eq0 = (tlen == pw[0].len);
            if (eq0) {
                for (uint32_t i = 0; i < tlen; ++i) {
                    if (char_map[(uint8_t)tstart[i]] != (uint8_t)pw[0].str[i]) {
                        eq0 = false; break;
                    }
                }
            }
            matched = eq0 ? 1 : 0;
        }
    }
    return false;
}

static inline char* put_u64(char* p, uint64_t val) {
    if (!val) { *p++ = '0'; return p; }
    char tmp[24]; int i = 0;
    while (val) { tmp[i++] = (val % 10) + '0'; val /= 10; }
    while (i) *p++ = tmp[--i];
    return p;
}

static void* eval_queries(void* arg) {
    WorkerState* w = (WorkerState*)arg;
    w->out.reserve(2 * 1024 * 1024);
    std::vector<uint32_t> bufA, bufB, matches;
    bufA.reserve(65536); bufB.reserve(65536); matches.reserve(16384);
    char num[32];

    for (size_t q_idx = w->q_s; q_idx < w->q_e; ++q_idx) {
        const auto& q = queries[q_idx];
        const uint32_t* res = nullptr;
        uint32_t res_len = 0;

        if (!q.is_phrase) {
            uint32_t t0 = q.terms[0];
            const uint32_t* cur_p = global_postings + postings_offsets[t0];
            uint32_t cur_len = postings_offsets[t0 + 1] - postings_offsets[t0];
            std::vector<uint32_t>* cur_v = nullptr;
            for (uint32_t j = 1; j < q.n_terms; ++j) {
                uint32_t tj = q.terms[j];
                const uint32_t* r_p = global_postings + postings_offsets[tj];
                uint32_t r_len = postings_offsets[tj + 1] - postings_offsets[tj];
                std::vector<uint32_t>& nxt = (cur_v == &bufA) ? bufB : bufA;
                if (q.ops[j - 1] == OP_AND) intersect(cur_p, cur_len, r_p, r_len, nxt);
                else if (q.ops[j - 1] == OP_OR) union_op(cur_p, cur_len, r_p, r_len, nxt);
                else diff_op(cur_p, cur_len, r_p, r_len, nxt);
                cur_v = &nxt; cur_p = cur_v->data(); cur_len = cur_v->size();
            }
            res = cur_p; res_len = cur_len;
        } else {
            std::pair<uint32_t, uint32_t> sorted_terms[8];
            for (uint32_t j = 0; j < q.n_terms; ++j) {
                uint32_t tj = q.terms[j];
                uint32_t len = postings_offsets[tj + 1] - postings_offsets[tj];
                sorted_terms[j] = {len, tj};
            }
            std::sort(sorted_terms, sorted_terms + q.n_terms);

            uint32_t t0 = sorted_terms[0].second;
            const uint32_t* cur_p = global_postings + postings_offsets[t0];
            uint32_t cur_len = sorted_terms[0].first;
            std::vector<uint32_t>* cur_v = nullptr;

            for (uint32_t j = 1; j < q.n_terms && cur_len > 0; ++j) {
                uint32_t tj = sorted_terms[j].second;
                const uint32_t* r_p = global_postings + postings_offsets[tj];
                uint32_t r_len = sorted_terms[j].first;
                std::vector<uint32_t>& nxt = (cur_v == &bufA) ? bufB : bufA;
                intersect(cur_p, cur_len, r_p, r_len, nxt);
                cur_v = &nxt; cur_p = cur_v->data(); cur_len = cur_v->size();
            }

            matches.clear();
            if (cur_len > 0) {
                PhraseWord pw[8];
                for (uint32_t j = 0; j < q.n_terms; ++j) {
                    uint32_t tid = q.terms[j];
                    pw[j] = {terms_info[tid].len, terms_info[tid].str};
                }
                for (uint32_t i = 0; i < cur_len; ++i) {
                    if (verify_phrase_fast(cur_p[i], pw, q.n_terms)) {
                        matches.push_back(cur_p[i]);
                    }
                }
            }
            res = matches.data(); res_len = matches.size();
        }

        uint64_t sum = 0;
        for (uint32_t i = 0; i < res_len; ++i) sum += res[i];
        char* pt = num;
        pt = put_u64(pt, res_len); *pt++ = ' ';
        pt = put_u64(pt, sum);
        w->out.append(num, pt - num);
        uint32_t top = std::min(res_len, 10u);
        for (uint32_t i = 0; i < top; ++i) {
            w->out += ' ';
            pt = put_u64(num, res[i]);
            w->out.append(num, pt - num);
        }
        w->out += '\n';
    }
    return nullptr;
}

int main(int argc, char** argv) {
    init_map();
    std::string data_dir = "/data", out_dir = "/out";
    for (int i = 1; i < argc; ++i) {
        if (std::string(argv[i]) == "--data" && i + 1 < argc) data_dir = argv[++i];
        if (std::string(argv[i]) == "--out" && i + 1 < argc) out_dir = argv[++i];
    }
    parse_queries(data_dir + "/queries.txt");

    int fd = open((data_dir + "/docs.txt").c_str(), O_RDONLY);
    if (fd < 0) return 1;
    struct stat st; fstat(fd, &st);
    size_t file_sz = st.st_size;
    const char* corpus = (const char*)mmap(nullptr, file_sz, PROT_READ, MAP_SHARED, fd, 0);
    close(fd);
    global_corpus = corpus;

    size_t chunk_sz = file_sz / N_THREADS;
    for (int i = 0; i < N_THREADS; ++i) {
        size_t s = i * chunk_sz, e = (i == N_THREADS - 1) ? file_sz : (i + 1) * chunk_sz;
        if (i > 0) while (s < file_sz && corpus[s - 1] != '\n') s++;
        if (i < N_THREADS - 1) while (e < file_sz && corpus[e - 1] != '\n') e++;
        workers[i].start = corpus + s; workers[i].end = corpus + e;
        workers[i].counts.assign(num_terms, 0);
        workers[i].last_doc.assign(num_terms, 0);
        workers[i].write_ptrs.assign(num_terms, nullptr);
    }

    pthread_t ths[N_THREADS];
    for (int i = 0; i < N_THREADS; ++i) pthread_create(&ths[i], nullptr, scan_pass1, &workers[i]);
    for (int i = 0; i < N_THREADS; ++i) pthread_join(ths[i], nullptr);

    uint32_t running = 0;
    postings_offsets[0] = 0;
    for (uint32_t t = 0; t < num_terms; ++t) {
        uint32_t t_tot = 0;
        for (int i = 0; i < N_THREADS; ++i) t_tot += workers[i].counts[t];
        postings_offsets[t] = running;
        running += t_tot;
    }
    postings_offsets[num_terms] = running;
    global_postings = (uint32_t*)malloc(running * sizeof(uint32_t));

    for (uint32_t t = 0; t < num_terms; ++t) {
        uint32_t cur = postings_offsets[t];
        for (int i = 0; i < N_THREADS; ++i) {
            workers[i].write_ptrs[t] = global_postings + cur;
            cur += workers[i].counts[t];
        }
    }

    for (int i = 0; i < N_THREADS; ++i) pthread_create(&ths[i], nullptr, scan_pass2, &workers[i]);
    for (int i = 0; i < N_THREADS; ++i) pthread_join(ths[i], nullptr);

    for (int i = 0; i < N_THREADS; ++i) {
        workers[i].counts.clear(); workers[i].counts.shrink_to_fit();
        workers[i].last_doc.clear(); workers[i].last_doc.shrink_to_fit();
        workers[i].write_ptrs.clear(); workers[i].write_ptrs.shrink_to_fit();
    }
    malloc_trim(0);

    size_t q_chunk = (queries.size() + N_THREADS - 1) / N_THREADS;
    for (int i = 0; i < N_THREADS; ++i) {
        workers[i].q_s = std::min(i * q_chunk, queries.size());
        workers[i].q_e = std::min((i + 1) * q_chunk, queries.size());
        pthread_create(&ths[i], nullptr, eval_queries, &workers[i]);
    }
    for (int i = 0; i < N_THREADS; ++i) pthread_join(ths[i], nullptr);

    mkdir(out_dir.c_str(), 0777);
    int out_fd = open((out_dir + "/results.txt").c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0666);
    for (int i = 0; i < N_THREADS; ++i) write(out_fd, workers[i].out.data(), workers[i].out.size());
    close(out_fd);
    return 0;
}
