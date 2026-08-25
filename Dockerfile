# u_crawler calendar-sync cron image
#
# Builds the `calendar` flow only: `--no-default-features` drops the `zoom`
# cargo feature (chromiumoxide, the headless browser, rusqlite) so the build
# needs neither network+git for chromiumoxide nor a browser at runtime — see
# AGENTS.md "Building without Zoom" and docs/specs/calendar-sync-flow.md
# ("Docker" under Further Notes).
#
# Credentials and config are injected at *runtime* via a mounted volume, not
# baked into this image (see docker-compose.yml / .env.example). Never COPY a
# real config.toml or token into this Dockerfile.

# ---- builder -----------------------------------------------------------
FROM rust:1-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY assets ./assets

RUN cargo build --release --locked --no-default-features && \
    strip target/release/u_crawler

# ---- runtime -------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# Links the GHCR package back to this repo. `.github/workflows/container.yml`
# also injects this via docker/metadata-action, which overrides what is set
# here; the LABEL is what a plain local `docker build` gets.
LABEL org.opencontainers.image.source="https://github.com/belcaik/unab-sync-content"

ARG PUID=1000
ARG PGID=1000

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        cron \
        tzdata && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd -g "${PGID}" appuser && \
    useradd -u "${PUID}" -g "${PGID}" -m -d /home/appuser -s /bin/sh appuser

COPY --from=builder /app/target/release/u_crawler /usr/local/bin/u_crawler
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
COPY docker/run-calendar.sh /usr/local/bin/run-calendar.sh
RUN chmod +x /usr/local/bin/u_crawler /usr/local/bin/entrypoint.sh /usr/local/bin/run-calendar.sh

# The config file (with credentials) lives here — mount it at runtime, do not
# bake one in. `u_crawler` resolves this via `$HOME/.config/u_crawler`.
ENV HOME=/home/appuser
# `caldir_root` in the mounted config.toml must point inside the container at
# this path (or wherever you mount the caldir volume below).
VOLUME ["/home/appuser/Caldir"]

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
