FROM gcc:15@sha256:eb5e64286ad47187b32bda75c6b6916166ecb41da56559b8b09e0001748d2cce
COPY main.cpp /src/main.cpp
RUN g++ -O3 -march=x86-64-v3 -std=c++20 -DNDEBUG -pthread -static-libstdc++ -static-libgcc -s -o /solution /src/main.cpp
USER 65534:65534
ENTRYPOINT ["/solution"]
