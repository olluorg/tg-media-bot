# syntax=docker/dockerfile:1

ARG DENO_VERSION=2.5.6
ARG FFMPEG_VERSION=7.1.1
# YouTube needs a JavaScript runtime for yt-dlp to solve its player challenges.
# Deno runs only while a YouTube page is being extracted (~300 MB for a few
# seconds). Build with --build-arg INSTALL_DENO=false on a very small server
# if you don't need YouTube in full quality.
ARG INSTALL_DENO=true

FROM denoland/deno:bin-${DENO_VERSION} AS deno

# Static ffmpeg/ffprobe: ~200 MB lighter than Debian's package and newer.
FROM mwader/static-ffmpeg:${FFMPEG_VERSION} AS ffmpeg

# ---- build ------------------------------------------------------------------
FROM rust:1-slim-bookworm AS build
WORKDIR /src
# Ride out flaky DNS/network in build sandboxes.
ENV CARGO_NET_RETRY=10
# Build dependencies first so source edits don't rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

# ---- runtime ----------------------------------------------------------------
FROM python:3.12-slim-bookworm
ARG INSTALL_DENO

RUN --mount=type=bind,from=deno,source=/deno,target=/tmp/deno \
    if [ "$INSTALL_DENO" = "true" ]; then install -m755 /tmp/deno /usr/local/bin/deno; fi

RUN useradd --uid 1000 --create-home --shell /usr/sbin/nologin app \
    && python -m venv /opt/yt-dlp \
    && /opt/yt-dlp/bin/pip install --no-cache-dir "yt-dlp[default]" \
    && chown -R app:app /opt/yt-dlp \
    && mkdir -p /data/tmp /cookies /plugins \
    && chown -R app:app /data /cookies /plugins

COPY --from=ffmpeg /ffmpeg /ffprobe /usr/local/bin/
COPY --from=build /src/target/release/tg-media-bot /usr/local/bin/tg-media-bot

ENV PATH=/opt/yt-dlp/bin:$PATH \
    DATA_DIR=/data \
    TEMP_DIR=/data/tmp \
    YTDLP_UPDATE_CMD="/opt/yt-dlp/bin/pip install --no-cache-dir -q -U yt-dlp[default]" \
    PYTHONDONTWRITEBYTECODE=1 \
    MALLOC_ARENA_MAX=2

USER app
WORKDIR /data
VOLUME ["/data"]
ENTRYPOINT ["/usr/local/bin/tg-media-bot"]
