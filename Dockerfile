FROM rust:1.94.1-slim-bookworm AS builder
WORKDIR /workspace/platform

RUN rustup target add wasm32-unknown-unknown
RUN cargo install wasm-bindgen-cli --version 0.2.114 --locked

COPY platform ./

RUN cargo run --locked -p xtask -- build-web --out-dir /tmp/app-web-dist
RUN cargo build --release -p app-server --locked

FROM debian:bookworm-slim AS runtime
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

ENV NODE_ENV=production
ENV APP_SERVER_BIND_ADDR=0.0.0.0:3000
ENV APP_SERVER_STATIC_DIR=/app/static

COPY --from=builder /workspace/platform/target/release/app-server /app/app-server
COPY --from=builder /tmp/app-web-dist /app/static

EXPOSE 3000

CMD ["/app/app-server"]
