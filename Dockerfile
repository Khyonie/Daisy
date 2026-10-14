ARG OLLAMA_VERSION=0.33.2
FROM ollama/ollama:${OLLAMA_VERSION} AS ollama

FROM rust:1.98-trixie AS chef
RUN cargo install cargo-chef --locked
RUN apt-get update \
    && apt-get install --yes --no-install-recommends libprotobuf-dev libssl-dev pkg-config protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app

FROM node:24-trixie-slim AS frontend
WORKDIR /app/docllm-frontend
COPY docllm-frontend/package.json docllm-frontend/package-lock.json ./
RUN npm ci
COPY docllm-frontend ./
RUN npm run build

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY docllm-backend ./docllm-backend
COPY prompts ./prompts
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked --workspace --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY docllm-backend ./docllm-backend
COPY prompts ./prompts
RUN cargo build --release --locked --package documentllm-server

FROM debian:trixie-slim AS runtime
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl tini libssl3t64 libgomp1 libopenblas0 libvulkan1 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/documentllm-server /usr/local/bin/
COPY --from=ollama /bin/ollama /usr/local/bin/ollama
COPY --from=ollama /usr/lib/ollama /usr/local/lib/ollama
COPY --from=frontend /app/docllm-frontend/dist ./docllm-frontend/dist
COPY docker/entrypoint.sh docker/healthcheck.sh /usr/local/bin/
RUN chmod 755 /usr/local/bin/entrypoint.sh /usr/local/bin/healthcheck.sh \
    && useradd --uid 10001 --create-home documentllm \
    && mkdir -p /data/upload /data/models /data/fastembed \
    && ln -s /data/upload /app/upload \
    && chown -R documentllm:documentllm /data
ENV DOCUMENTLLM_BIND_ADDRESS=0.0.0.0:3001 \
    DOCUMENTLLM_DATABASE_PATH=/data/database.lancedb \
    DOCUMENTLLM_EMBEDDING_CACHE_PATH=/data/fastembed \
    DOCUMENTLLM_MODEL=gemma4:e4b \
    DOCUMENTLLM_OLLAMA_URL=http://127.0.0.1:11434 \
    OLLAMA_HOST=127.0.0.1:11434 \
    OLLAMA_MODELS=/data/models \
    OLLAMA_CONTEXT_LENGTH=8192 \
    OLLAMA_NUM_PARALLEL=1 \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility \
    NVIDIA_VISIBLE_DEVICES=all
USER documentllm
EXPOSE 3001
VOLUME ["/data"]
HEALTHCHECK --interval=30s --timeout=10s --start-period=30m --retries=3 CMD ["healthcheck.sh"]
ENTRYPOINT ["/usr/bin/tini", "-g", "--", "/usr/local/bin/entrypoint.sh"]
CMD ["documentllm-server"]
