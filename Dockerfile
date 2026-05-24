# syntax=docker/dockerfile:1

FROM --platform=$BUILDPLATFORM node:22-bookworm-slim AS frontend-builder
WORKDIR /app/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM rust:1-bookworm AS backend-builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM gcr.io/distroless/cc-debian12
WORKDIR /app
ENV TZ=Asia/Shanghai \
    EMBYPANEL_API_ADDR=0.0.0.0:8090
COPY --from=backend-builder /app/target/release/emby302gateway-rs /app/emby302gateway-rs
COPY --from=frontend-builder /app/frontend/dist /app/frontend/dist
COPY data/config.toml.example /data/config.toml.example
VOLUME ["/data"]
EXPOSE 8090 8091 8092 8093 8094 8095
ENTRYPOINT ["/app/emby302gateway-rs"]
