# syntax=docker/dockerfile:1.7

# ---------------------------------------------------------------------------
# Stage 1: build
# ---------------------------------------------------------------------------
# Need Rust >= 1.85 (some crates require edition2024 / rustc 1.85+)
FROM rust:bookworm AS builder

# Pin upstream for reproducible builds
# Override: docker build --build-arg VOCA_GIT_SHA=<sha> .
ARG VOCA_GIT_SHA=03f91ae073fc

WORKDIR /src

RUN git clone https://github.com/sultson/voca.git /src/voca \
    && cd /src/voca \
    && git checkout "${VOCA_GIT_SHA}"

COPY Cargo.toml /src/voca-server/Cargo.toml
COPY src /src/voca-server/src

WORKDIR /src/voca-server

# Point the path dependency at the cloned upstream
RUN sed -i 's|path = "../voca/crates/tiny-voca"|path = "/src/voca/crates/tiny-voca"|' Cargo.toml \
    && cat Cargo.toml

RUN cargo build --release

# ---------------------------------------------------------------------------
# Stage 2: runtime
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -u 1000 -m -d /home/voca voca

WORKDIR /app

COPY --from=builder /src/voca-server/target/release/voca-server /usr/local/bin/voca-server

# Model data (~19 MB)
RUN mkdir -p /models \
    && curl -fsSL https://github.com/sultson/voca/releases/download/v0.1.0/voca-data.tar.gz \
       | tar -xz -C /models \
    && chown -R voca:voca /models

USER voca

ENV VOCA_HOME=/models \
    MODELS_DIR=/models \
    HOST=0.0.0.0 \
    PORT=3420 \
    RUST_LOG=voca_server=info,tower_http=info \
    API_KEY= \
    REQUIRE_API_KEY=0 \
    DEFAULT_VOICE=F2

EXPOSE 3420

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:3420/health || exit 1

ENTRYPOINT ["/usr/local/bin/voca-server"]
