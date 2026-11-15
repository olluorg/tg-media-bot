# syntax=docker/dockerfile:1

ARG FFMPEG_VERSION=7.1.1

# Static ffmpeg/ffprobe: ~200 MB lighter than Debian's package and newer.
FROM mwader/static-ffmpeg:${FFMPEG_VERSION} AS ffmpeg

# ---- build ------------------------------------------------------------------
FROM rust:1-slim-bookworm AS build
WORKDIR /src
# Ride out flaky DNS/network in build sandboxes and compile for modern x86-64-v3 (AVX2, BMI2).
ENV CARGO_NET_RETRY=10 \
    RUSTFLAGS="-C target-cpu=x86-64-v3"
# Build dependencies first so source edits don't rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

# ---- runtime ----------------------------------------------------------------
FROM python:3.12-slim-bookworm

# QuickJS: ultra-lightweight JS engine for YouTube player challenges (~5 MB RAM vs ~300 MB with Deno).
RUN apt-get update && apt-get install -y --no-install-recommends quickjs \
    && rm -rf /var/lib/apt/lists/* \
    && (ln -s /usr/bin/qjs /usr/local/bin/quickjs 2>/dev/null || true)

RUN useradd --uid 1000 --create-home --shell /usr/sbin/nologin app \
    && python -m venv /opt/yt-dlp \
    && /opt/yt-dlp/bin/pip install --no-cache-dir "yt-dlp[default]" \
    && /opt/yt-dlp/bin/python -m compileall -q /opt/yt-dlp \
    && chown -R app:app /opt/yt-dlp \
    && mkdir -p /data/tmp /cookies /plugins \
    && chown -R app:app /data /cookies /plugins

COPY --from=ffmpeg /ffmpeg /ffprobe /usr/local/bin/
COPY --from=build /src/target/release/tg-media-bot /usr/local/bin/tg-media-bot

ENV PATH=/opt/yt-dlp/bin:$PATH \
    DATA_DIR=/data \
    TEMP_DIR=/data/tmp \
    YTDLP_UPDATE_CMD="/opt/yt-dlp/bin/pip install --no-cache-dir -q -U yt-dlp[default] && /opt/yt-dlp/bin/python -m compileall -q /opt/yt-dlp" \
    MALLOC_ARENA_MAX=2

USER app
WORKDIR /data
VOLUME ["/data"]
ENTRYPOINT ["/usr/local/bin/tg-media-bot"]
