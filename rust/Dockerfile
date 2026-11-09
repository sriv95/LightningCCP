# Build from the rust/ directory: docker build -f rust/Dockerfile rust
FROM rust:1.99.1-alpine AS build
# gcc + musl-dev: the bundled SQLite (libsqlite3-sys) and ring compile C. Alpine's default host target is musl.
RUN apk add --no-cache gcc musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY assets ./assets
COPY src ./src
COPY benches ./benches
# Cargo.toml has lto = true (fat). Fat LTO needs more than about 4 GiB of RAM: on a smaller build host pass
# `--build-arg CARGO_PROFILE_RELEASE_LTO=thin` (a little larger and slower binary, same behavior).
ARG CARGO_PROFILE_RELEASE_LTO=true
ENV CARGO_PROFILE_RELEASE_LTO=$CARGO_PROFILE_RELEASE_LTO
RUN cargo build --release --locked --bin lightningccp
# /app/data holds the SQLite database; compose mounts a named volume here.
RUN mkdir -p /app/data

# No CA certificates: reqwest (rustls-tls) and serenity/tungstenite (rustls_backend) use bundled webpki-roots.
FROM scratch
COPY --from=build /src/target/release/lightningccp /lightningccp
COPY --from=build --chown=10001:10001 /app/data /app/data
ENV DB_PATH=/app/data/bot.db
WORKDIR /app
USER 10001:10001
ENTRYPOINT ["/lightningccp"]
