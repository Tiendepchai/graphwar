FROM rust:1.89-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
RUN rustup target add wasm32-unknown-unknown \
    && cargo install --locked wasm-bindgen-cli --version 0.2.126 \
    && cargo build --locked --release -p graphwar-server \
    && cargo build --locked --release -p graphwar-client-wasm --target wasm32-unknown-unknown \
    && mkdir -p /app/web \
    && wasm-bindgen \
        --target web \
        --out-dir /app/web \
        --out-name graphwar_client_wasm \
        /app/target/wasm32-unknown-unknown/release/graphwar_client_wasm.wasm

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/graphwar-server /usr/local/bin/graphwar-server
COPY assets ./assets
COPY --from=builder /app/web ./assets/web
COPY migrations ./migrations
ENV GRAPHWAR_STATIC_DIR=/app/assets/web
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["graphwar-server"]
