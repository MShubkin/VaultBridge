# Multi-stage сборка api-gateway. Без libpq: diesel-async (postgres_backend +
# tokio-postgres). Рантайм — slim debian с ca-certificates (TLS к Redis/Postgres).

FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
RUN cargo build --release -p api-gateway

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/api-gateway /usr/local/bin/api-gateway
ENV PORT=8080
EXPOSE 8080
CMD ["api-gateway"]
