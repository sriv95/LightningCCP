# Build from the repo root: docker build -t lightningccp .
FROM rust:1.99.1-alpine AS build
# gcc + musl-dev: the bundled SQLite (libsqlite3-sys) and ring compile C. Alpine's default host target is musl.
RUN apk add --no-cache gcc musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY assets ./assets
COPY src ./src
COPY benches ./benches
# Cargo.toml has lto = true (fat) for native builds. Images default to thin: fat LTO needs more than about 4 GiB of
# build memory and is killed on a 3.8 GiB VM. On a bigger host: `--build-arg CARGO_PROFILE_RELEASE_LTO=true`.
ARG CARGO_PROFILE_RELEASE_LTO=thin
ENV CARGO_PROFILE_RELEASE_LTO=$CARGO_PROFILE_RELEASE_LTO
RUN cargo build --release --locked --bin lightningccp
# /app/data holds the SQLite database; compose mounts a named volume here.
RUN mkdir -p /app/data

# 1000:1000 is the uid:gid of the old Python image's `bot` user, so the existing bot-data volume stays writable.
# No CA certificates: reqwest (rustls-tls) and serenity/tungstenite (rustls_backend) use bundled webpki-roots.
FROM scratch
COPY --from=build /src/target/release/lightningccp /lightningccp
COPY --from=build --chown=1000:1000 /app/data /app/data
ENV DB_PATH=/app/data/bot.db
WORKDIR /app
USER 1000:1000
ENTRYPOINT ["/lightningccp"]
