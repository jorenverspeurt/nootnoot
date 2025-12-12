# ---------- Builder stage ----------
FROM rust:1.91-bookworm AS builder

WORKDIR /app

# Cache dependencies
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY config ./config
COPY static ./static
COPY packaging ./packaging

RUN cargo build --release

# ---------- Runtime stage ----------
FROM debian:bookworm-slim AS runtime

# Install ping and CA certs
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      ca-certificates \
      iputils-ping && \
    rm -rf /var/lib/apt/lists/*

# Non-root user
RUN useradd -m -u 10001 nootuser

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/nootnoot /usr/local/bin/nootnoot

# Default config (can be overridden by mounting another file)
COPY ./config/nootnoot.docker.toml /etc/nootnoot.toml

# Directory for optional file logs
RUN mkdir -p /var/log/nootnoot && chown -R nootuser:nootuser /var/log/nootnoot

USER nootuser

EXPOSE 8080
ENV RUST_LOG=info

# Default: use config file, but you can override at docker run time
ENTRYPOINT ["nootnoot"]
CMD ["--config", "/etc/nootnoot.toml"]
