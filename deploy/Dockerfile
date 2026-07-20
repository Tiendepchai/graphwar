FROM rust:1.89-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --locked --release -p graphwar-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/graphwar-server /usr/local/bin/graphwar-server
COPY assets ./assets
COPY migrations ./migrations
ENV GRAPHWAR_STATIC_DIR=/app/assets
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["graphwar-server"]
