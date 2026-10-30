# track-c build image: modern gcc on Ubuntu 24.04.
FROM gcc:16

WORKDIR /app

# Copy only the sources needed for the build.
COPY track-c/c_join.cpp /app/c_join.cpp

# Prebuild the binary so container start just runs it.
RUN g++ -O3 -march=native -std=c++20 -pthread -fno-exceptions -fno-rtti \
    -static-libstdc++ -static-libgcc \
    -o /app/c_join /app/c_join.cpp

ENTRYPOINT ["/app/c_join"]
CMD ["data", "out"]
