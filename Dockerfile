FROM rust:1.85-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential libssl-dev pkg-config protobuf-compiler && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY rs/ rs/
RUN cargo build --release -p mindojo-server && \
    find target -name "libonnxruntime*.so*" -exec cp {} /usr/lib/ \;

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 curl && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd -r mindojo && useradd -r -g mindojo -d /data mindojo
COPY --from=builder /usr/lib/libonnxruntime*.so* /usr/lib/
COPY --from=builder /src/target/release/mindojo /usr/local/bin/
RUN ldconfig
USER mindojo
EXPOSE 8191
HEALTHCHECK --interval=30s --timeout=5s --retries=3 --start-period=120s \
    CMD curl -sf http://localhost:8191/health || exit 1
ENTRYPOINT ["mindojo", "serve"]
