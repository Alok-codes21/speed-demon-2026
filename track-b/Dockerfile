# Build with GCC 15, tuned for the scoring CPU, then ship the binary alone.
FROM gcc:15@sha256:eb5e64286ad47187b32bda75c6b6916166ecb41da56559b8b09e0001748d2cce AS build
WORKDIR /src
COPY main.cpp .
RUN g++ -std=c++20 -O3 -flto -march=x86-64-v3 -DNDEBUG -pthread -static-libstdc++ -static-libgcc main.cpp -o /solution

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /solution /solution
ENTRYPOINT ["/solution"]
