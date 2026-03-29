FROM rust:1.94.1-slim-bookworm AS builder
WORKDIR /workspace/platform

RUN apt-get update \
    && apt-get install -y --no-install-recommends binaryen \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-unknown-unknown
RUN cargo install wasm-bindgen-cli --version 0.2.115 --locked

COPY platform ./

RUN XTASK_SKIP_WASM_OPT=1 cargo run --locked -p xtask -- build-web --out-dir /tmp/app-web-dist
RUN cargo build --release -p app-server --locked

FROM debian:bookworm-slim AS runtime
ARG APP_UID=10001
ARG APP_GID=10001

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && groupadd --gid "${APP_GID}" app \
    && useradd --uid "${APP_UID}" --gid "${APP_GID}" --create-home --home-dir /app --shell /usr/sbin/nologin app \
    && mkdir -p /app/static /tmp \
    && chown -R "${APP_UID}:${APP_GID}" /app /tmp \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

ENV NODE_ENV=production \
    APP_SERVER_BIND_ADDR=0.0.0.0:3000 \
    APP_SERVER_STATIC_DIR=/app/static

COPY --from=builder --chown=10001:10001 /workspace/platform/target/release/app-server /app/app-server
COPY --from=builder --chown=10001:10001 /tmp/app-web-dist /app/static

USER 10001:10001

EXPOSE 3000

ENTRYPOINT ["/app/app-server"]
