# --- web UI build ---
FROM node:24-slim AS web
WORKDIR /app
COPY web/package.json web/package-lock.json web/
RUN cd web && npm ci
COPY buf.yaml buf.gen.yaml ./
COPY proto proto
COPY web web
RUN cd web && npm run gen && npm run build

# --- server build (build.rs fetches protoc automatically) ---
FROM rust:1.92-bookworm AS server
WORKDIR /app
COPY proto proto
COPY server server
RUN cd server && cargo build --release

# --- runtime ---
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=server /app/server/target/release/aliceandbob-server /usr/local/bin/aliceandbob-server
COPY --from=web /app/web/dist /app/web-dist

# Mount providers.toml at /app/providers.toml and a volume at /data.
ENV RUST_LOG=info
EXPOSE 3030
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD curl -sf http://localhost:3030/health || exit 1
CMD ["aliceandbob-server"]
