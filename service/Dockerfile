FROM rust:1.92-bookworm AS builder

WORKDIR /build
COPY . .
RUN cargo build --locked --release

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 nga-reminder \
    && mkdir -p /app/data /data \
    && chown -R nga-reminder:nga-reminder /app /data
USER nga-reminder
WORKDIR /app

COPY --from=builder /build/target/release/nga-reminder /usr/local/bin/nga-reminder

EXPOSE 12888
ENTRYPOINT ["nga-reminder"]
CMD ["all"]
