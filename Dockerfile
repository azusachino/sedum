# syntax=docker/dockerfile:1

# ---- Build stage ----
FROM rust:1-slim-bookworm AS builder

# ring/cc (via sqlx rustls) need a C toolchain; pkg-config for build scripts.
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Warm the dependency layer with a stub crate so deps only rebuild when the
# manifests change, not on every source edit.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && echo '' > src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src \
        target/release/miku target/release/deps/miku-* \
        target/release/deps/libmiku-* target/release/.fingerprint/miku-*

# Real sources. migrations/ is embedded at compile time by sqlx::migrate!.
COPY src ./src
COPY migrations ./migrations
COPY static ./static
RUN cargo build --release --locked --bin miku

# ---- Runtime stage ----
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system miku \
    && useradd --system --gid miku miku

WORKDIR /app

# Binary + the assets the app loads from CWD at runtime (templates via
# minijinja path_loader, static via ServeDir). The miku/ content dir is
# provided at runtime as a bind mount. The binary lives on PATH so it does
# not collide with the /app/miku content directory.
COPY --from=builder /build/target/release/miku /usr/local/bin/miku
COPY src/templates /app/src/templates
COPY static /app/static

RUN mkdir -p /app/miku && chown -R miku:miku /app
USER miku

EXPOSE 3000
CMD ["miku"]
