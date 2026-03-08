FROM rust:slim-trixie AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential libssl-dev pkg-config protobuf-compiler libprotobuf-dev && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /src

# Layer 1: Cache dependency builds.
# Copy only manifests and lock file, create stub sources, build deps.
COPY Cargo.toml Cargo.lock ./
COPY rs/memcan-core/Cargo.toml rs/memcan-core/Cargo.toml
COPY rs/memcan-server/Cargo.toml rs/memcan-server/Cargo.toml
COPY rs/memcan/Cargo.toml rs/memcan/Cargo.toml
RUN mkdir -p rs/memcan-core/src rs/memcan-server/src rs/memcan/src && \
    echo "// stub" > rs/memcan-core/src/lib.rs && \
    echo "fn main() {}" > rs/memcan-server/src/main.rs && \
    echo "fn main() {}" > rs/memcan/src/main.rs && \
    echo "fn main() {}" > rs/memcan-server/build.rs && \
    cargo build --release -p memcan-server 2>/dev/null || true && \
    rm -rf rs/

# Layer 2: Build actual source (deps already cached).
COPY rs/ rs/
RUN cargo build --release -p memcan-server && \
    find target -name "libonnxruntime*.so*" -exec cp {} /usr/lib/ \;

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3t64 curl && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd -r memcan && useradd -r -g memcan -d /data memcan && \
    mkdir -p /data/lancedb /data/fastembed && chown -R memcan:memcan /data
COPY --from=builder /usr/lib/libonnxruntime*.so* /usr/lib/
COPY --from=builder /src/target/release/memcan-server /usr/local/bin/
RUN ldconfig
USER memcan
EXPOSE 8191
HEALTHCHECK --interval=30s --timeout=5s --retries=3 --start-period=120s \
    CMD curl -sf http://localhost:8191/health || exit 1
STOPSIGNAL SIGTERM
ENTRYPOINT ["memcan-server", "serve"]
