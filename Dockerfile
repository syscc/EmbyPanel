# syntax=docker/dockerfile:1

FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS frontend-builder
WORKDIR /app/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM --platform=$BUILDPLATFORM rust:1-bookworm AS backend-builder
WORKDIR /app
ARG TARGETARCH
RUN apt-get update \
    && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
    && rm -rf /var/lib/apt/lists/*
RUN if [ "$TARGETARCH" = "arm64" ]; then rustup target add aarch64-unknown-linux-gnu; fi
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN if [ "$TARGETARCH" = "arm64" ]; then \
      CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
      AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
      CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
      cargo build --release --locked --target aarch64-unknown-linux-gnu \
      && cp target/aarch64-unknown-linux-gnu/release/emby302gateway-rs /app/emby302gateway-rs; \
    else \
      cargo build --release --locked \
      && cp target/release/emby302gateway-rs /app/emby302gateway-rs; \
    fi

FROM --platform=$TARGETPLATFORM gcr.io/distroless/cc-debian12
WORKDIR /app
ENV TZ=Asia/Shanghai \
    EMBYPANEL_API_ADDR=0.0.0.0:8090
COPY --from=backend-builder /app/emby302gateway-rs /app/emby302gateway-rs
COPY --from=frontend-builder /app/frontend/dist /app/frontend/dist
COPY data/config.toml.example /data/config.toml.example
VOLUME ["/data"]
EXPOSE 8090 8091 8092 8093 8094 8095
ENTRYPOINT ["/app/emby302gateway-rs"]
