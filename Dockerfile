# Build stage
FROM rust:1-slim-bookworm AS builder

RUN apt-get update \
    && apt-get install --no-install-recommends -y pkg-config libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY langpacks ./langpacks
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates libsqlite3-0 \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 1000 heroku \
    && mkdir -p /data \
    && chown heroku:heroku /data

COPY --from=builder /src/target/release/heroku /usr/local/bin/heroku

USER heroku
WORKDIR /data
ENV HEROKU_DATA_ROOT=/data
VOLUME ["/data"]

CMD ["heroku"]
