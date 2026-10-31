# PGO build: train on a synthetic corpus with the generator's shape (token
# lengths, query mix, skew), then rebuild with the profile. One stage.
FROM halloumee/speed-demon-rust:1.99.1-trixie-llvm-tools@sha256:13e4ee7135d8eac6c56476c7d1aee201f36479f4745fd5d0e6b4529f4e13251a AS build
WORKDIR /src
COPY Cargo.toml .
COPY src src
RUN mkdir -p /data /out /pgotrain \
 && RUSTFLAGS="-C target-cpu=native -C profile-generate=/pgotrain" cargo build --release --offline \
 && ./target/release/gen 1 && ./target/release/index \
 && ./target/release/gen 7 && ./target/release/index \
 && "$(find /usr/local/rustup -name 'llvm-profdata*' -type f | head -1)" merge -o /pgotrain/merged.profdata /pgotrain \
 && touch src/main.rs \
 && RUSTFLAGS="-C target-cpu=native -C profile-use=/pgotrain/merged.profdata" cargo build --release --offline

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
ENV MALLOC_ARENA_MAX=2
COPY --from=build /src/target/release/index /app/solution
ENTRYPOINT ["/app/solution"]
