FROM rust:1.93.1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    clang \
    cmake \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy shit
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release --bin rdrive-server

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/rdrive-server /app/rdrive-server

COPY docker-entrypoint.sh /app/docker-entrypoint.sh

RUN chmod +x /app/docker-entrypoint.sh

# Create config and storage directories upfront
RUN mkdir -p /home/rdrive/.rdrive/storage

EXPOSE 3000

ENV LOG_LEVEL=debug
ENV HOME=/home/rdrive
ENV MAX_CONNECTIONS=128
ENV MAX_FILE_SIZE="12 * 1024 * 1024 * 1024"

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s CMD test -f /app/rdrive-server || exit 1

ENTRYPOINT ["/app/docker-entrypoint.sh"]

CMD ["serve"]
