# ---- builder ----
FROM rust:1-bookworm AS builder
WORKDIR /build

# 先以 Cargo.toml/Cargo.lock + 假 main 預編依賴，最大化 layer 快取
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

# 再放真實原始碼編譯
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ---- runtime ----
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /build/target/release/qwen2api-rs /app/qwen2api-rs
COPY web /app/web

ENV PORT=7860 \
    WEB_DIR=/app/web \
    DATA_DIR=/app/data \
    LOG_LEVEL=info

EXPOSE 7860
VOLUME ["/app/data"]

# 容器健康檢查（對齊 /healthz；PORT 由環境變數展開）
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- "http://127.0.0.1:${PORT}/healthz" >/dev/null 2>&1 || exit 1

CMD ["/app/qwen2api-rs"]
