FROM --platform=$BUILDPLATFORM rust:1.94.1-slim-bookworm@sha256:5ae2d2ef9875c9c2407bf9b5678e6375304f7ecf8ea46b23e403a5690ec357ec AS builder
WORKDIR /workspace/platform

ARG TARGETPLATFORM
ARG TARGETARCH
ARG BUILDARCH
ARG BINARYEN_VERSION=129

RUN test "$TARGETPLATFORM" = "linux/amd64"

RUN apt-get update \
    && apt-get install -y --no-install-recommends curl gcc-x86-64-linux-gnu libc6-dev-amd64-cross pkg-config tar \
    && rm -rf /var/lib/apt/lists/*

RUN case "$BUILDARCH" in \
      amd64) binaryen_arch="x86_64-linux"; binaryen_sha256="50b9fa62b9abea752da92ec57e0c555fee578760cd237c40107957715d2976ba" ;; \
      arm64) binaryen_arch="aarch64-linux"; binaryen_sha256="81d46b86b10876ab615eec67e09fcc5615115a7b189cfe3d466725ee36c46ac2" ;; \
      *) echo "unsupported BUILDARCH: $BUILDARCH" >&2; exit 1 ;; \
    esac \
    && curl -fsSL -o /tmp/binaryen.tar.gz "https://github.com/WebAssembly/binaryen/releases/download/version_${BINARYEN_VERSION}/binaryen-version_${BINARYEN_VERSION}-${binaryen_arch}.tar.gz" \
    && echo "${binaryen_sha256}  /tmp/binaryen.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/binaryen.tar.gz -C /tmp \
    && install -m 0755 "/tmp/binaryen-version_${BINARYEN_VERSION}/bin/wasm-opt" /usr/local/bin/wasm-opt \
    && rm -rf /tmp/binaryen.tar.gz "/tmp/binaryen-version_${BINARYEN_VERSION}"

RUN rustup target add wasm32-unknown-unknown
RUN rustup target add x86_64-unknown-linux-gnu
RUN cargo install wasm-bindgen-cli --version 0.2.115 --locked

COPY platform ./

RUN cargo run --locked -p xtask -- build-web --out-dir /tmp/app-web-dist
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc
RUN cargo build --release -p app-server --locked --target x86_64-unknown-linux-gnu

FROM debian:bookworm-slim@sha256:f06537653ac770703bc45b4b113475bd402f451e85223f0f2837acbf89ab020a AS runtime
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

COPY --from=builder --chown=10001:10001 /workspace/platform/target/x86_64-unknown-linux-gnu/release/app-server /app/app-server
COPY --from=builder --chown=10001:10001 /tmp/app-web-dist /app/static

USER 10001:10001

EXPOSE 3000

ENTRYPOINT ["/app/app-server"]
