# syntax=docker/dockerfile:1.7
FROM rust:1.92-bookworm AS builder

WORKDIR /build
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --locked --release --bin nga-reminder \
    && cp target/release/nga-reminder /usr/local/bin/nga-reminder

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 nga-reminder \
    && mkdir -p /app/data /data \
    && chown -R nga-reminder:nga-reminder /app /data
USER nga-reminder
WORKDIR /app

COPY --from=builder /usr/local/bin/nga-reminder /usr/local/bin/nga-reminder

EXPOSE 12888
ENTRYPOINT ["nga-reminder"]
CMD ["all"]
