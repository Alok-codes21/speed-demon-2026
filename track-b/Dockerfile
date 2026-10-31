# Build stage 1: PGO training data from the published generator. A
# byte-identical copy of the generator costs nothing on the S axis, and
# build-time training (PGO) on generator output is explicitly allowed.
FROM python:3.13-slim@sha256:d98cc29d3266e535dd9da1099688d9cb3ca4753813571e071763d17725460962 AS gen
COPY b_index.py /gen/b_index.py
RUN python3 /gen/b_index.py --seed 1 --scale sample --out /pgodata1 \
 && python3 /gen/b_index.py --seed 7 --scale sample --out /pgodata2 \
 && python3 /gen/b_index.py --seed 1 --scale tiny --out /pgodata3

# Build stage 2: PGO-optimised Rust build on the allowlisted llvm-tools image.
FROM halloumee/speed-demon-rust:1.99.1-trixie-llvm-tools@sha256:13e4ee7135d8eac6c56476c7d1aee201f36479f4745fd5d0e6b4529f4e13251a AS build
WORKDIR /src
COPY . .
COPY --from=gen /pgodata1 /pgodata1
COPY --from=gen /pgodata2 /pgodata2
COPY --from=gen /pgodata3 /pgodata3
RUN mkdir -p /pgo-data \
 && RUSTFLAGS="-C target-cpu=native -C profile-generate=/pgo-data" cargo build --release --offline \
 && ./target/release/solution /pgodata1 /tmp/o1 \
 && ./target/release/solution /pgodata2 /tmp/o2 \
 && ./target/release/solution /pgodata3 /tmp/o3 \
 && "$(find /usr/local/rustup -name 'llvm-profdata*' -type f | head -1)" merge -o /pgo-data/merged.profdata /pgo-data \
 && RUSTFLAGS="-C target-cpu=native -C profile-use=/pgo-data/merged.profdata" cargo build --release --offline

# Runtime stage: nothing but the binary. Reads /data, writes /out, exits 0.
FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /src/target/release/solution /app/solution
ENTRYPOINT ["/app/solution"]
