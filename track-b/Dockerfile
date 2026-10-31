# Build stage: allowlisted Rust toolchain, fully offline, zero dependencies.
FROM rust:1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb AS build
WORKDIR /src
COPY . .
# target-cpu=native is allowed: tracks that ask for it are built on the scoring box.
RUN RUSTFLAGS="-C target-cpu=native" cargo build --release --offline

# Runtime stage: nothing but the binary. Reads /data, writes /out, exits 0.
FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /src/target/release/solution /app/solution
ENTRYPOINT ["/app/solution"]
