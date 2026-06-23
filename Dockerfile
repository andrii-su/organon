# Organon — MCP server image.
#
# Bundles the Rust `organon` binary (core + CLI + MCP server) and the Python AI
# layer (embeddings, extraction, relation indexing) so all MCP tools work from a
# single image. Communicates over stdio by default.
#
# Build:   docker build -t organon .
# Index:   docker run --rm -v "$PWD:/workspace" -v organon-data:/data organon index /workspace
# Serve:   docker run -i --rm -v "$PWD:/workspace" -v organon-data:/data organon mcp --scope /workspace
#
# State (graph DB, vectors, embedding-model cache) lives under ORGANON_HOME=/data;
# mount a named volume there to persist it across runs. Mount your project at
# /workspace so the server can see your files.

# ── Stage 1: build the Rust binary ──────────────────────────────────────────────
FROM rust:1-slim-bookworm AS builder

# rusqlite's bundled SQLite compiles C, so a C toolchain is required.
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked --bin organon \
    && strip target/release/organon

# ── Stage 2: runtime with the Python AI layer ───────────────────────────────────
FROM python:3.12-slim-bookworm AS runtime

# onnxruntime (via fastembed) needs libgomp at runtime.
RUN apt-get update \
    && apt-get install -y --no-install-recommends libgomp1 \
    && rm -rf /var/lib/apt/lists/*

# uv for the Python layer.
COPY --from=ghcr.io/astral-sh/uv:latest /uv /usr/local/bin/uv

WORKDIR /app

# Install the Python AI layer (locked, no dev deps). The project itself is
# installed so `python -m ai.indexer` resolves regardless of working directory.
COPY pyproject.toml uv.lock ./
COPY ai ./ai
RUN uv sync --locked --no-dev

# The Rust binary.
COPY --from=builder /build/target/release/organon /usr/local/bin/organon

# All state (graph DB, vectors, embedding-model cache) under one mountable dir.
# The CLI shells out to `uv run --project $ORGANON_PYTHON_PROJECT` for the Python layer.
ENV ORGANON_HOME=/data \
    ORGANON_PYTHON_PROJECT=/app \
    HF_HOME=/data/hf-cache \
    FASTEMBED_CACHE_PATH=/data/fastembed-cache
RUN mkdir -p /data /workspace
VOLUME ["/data"]
WORKDIR /workspace

ENTRYPOINT ["organon"]
CMD ["mcp", "--scope", "/workspace"]
