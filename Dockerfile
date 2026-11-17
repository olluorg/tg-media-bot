# syntax=docker/dockerfile:1

ARG DENO_VERSION=2.5.6
ARG FFMPEG_VERSION=7.1.1
ARG ZAPRET_VERSION=v1.0.7

FROM denoland/deno:bin-${DENO_VERSION} AS deno
# Static ffmpeg/ffprobe: ~200 MB lighter than Debian's package and newer.
FROM mwader/static-ffmpeg:${FFMPEG_VERSION} AS ffmpeg

# ---- zapret2 binaries & assets (for dpi flavor) -----------------------------
FROM alpine:3.20 AS zapret
ARG TARGETARCH
ARG ZAPRET_VERSION
WORKDIR /tmp
RUN apk add --no-cache curl tar
RUN set -ex; \
    ARCH="${TARGETARCH:-}"; \
    if [ -z "$ARCH" ]; then \
        case "$(uname -m)" in \
            x86_64) ARCH="amd64" ;; \
            aarch64) ARCH="arm64" ;; \
            armv7l) ARCH="arm" ;; \
            *) ARCH="amd64" ;; \
        esac; \
    fi; \
    case "$ARCH" in \
        amd64) ARCH_DIR="linux-x86_64" ;; \
        arm64) ARCH_DIR="linux-arm64" ;; \
        arm)   ARCH_DIR="linux-arm" ;; \
        *) echo "Unsupported target architecture: $ARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o zapret.tar.gz "https://github.com/bol-van/zapret2/releases/download/${ZAPRET_VERSION}/zapret2-${ZAPRET_VERSION}.tar.gz"; \
    tar -xzf zapret.tar.gz --strip-components=1; \
    mkdir -p /opt/zapret2/lua /opt/zapret2/fake; \
    cp "binaries/${ARCH_DIR}/nfqws2" /opt/zapret2/nfqws2; \
    cp lua/*.lua /opt/zapret2/lua/; \
    cp files/fake/* /opt/zapret2/fake/; \
    chmod 755 /opt/zapret2/nfqws2; \
    chmod -R a+rX /opt/zapret2

# ---- build ------------------------------------------------------------------
FROM rust:1-slim-bookworm AS build
ARG TARGETARCH
WORKDIR /src
# Ride out flaky DNS/network in build sandboxes.
ENV CARGO_NET_RETRY=10

# Compile for modern x86-64-v3 (AVX2, BMI2) on amd64; use default native on arm64.
RUN if [ "$TARGETARCH" = "amd64" ] || [ -z "$TARGETARCH" ]; then \
        mkdir -p /root/.cargo && printf '[build]\nrustflags = ["-C", "target-cpu=x86-64-v3"]\n' > /root/.cargo/config.toml; \
    fi
# Build dependencies first so source edits don't rebuild them.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release --locked

# ---- base-runtime -----------------------------------------------------------
FROM python:3.12-slim-bookworm AS base-runtime

RUN useradd --uid 1000 --create-home --shell /usr/sbin/nologin app \
    && python -m venv /opt/yt-dlp \
    && /opt/yt-dlp/bin/pip install --no-cache-dir "yt-dlp[default]" \
    && /opt/yt-dlp/bin/python -m compileall -q /opt/yt-dlp \
    && chown -R app:app /opt/yt-dlp \
    && mkdir -p /data/tmp /cookies /plugins \
    && chown -R app:app /data /cookies /plugins

COPY --from=deno /deno /usr/local/bin/deno
COPY --from=ffmpeg /ffmpeg /ffprobe /usr/local/bin/
COPY --from=build /src/target/release/tg-media-bot /usr/local/bin/tg-media-bot

ENV PATH=/opt/yt-dlp/bin:$PATH \
    DATA_DIR=/data \
    TEMP_DIR=/data/tmp \
    YTDLP_UPDATE_CMD="/opt/yt-dlp/bin/pip install --no-cache-dir -q -U yt-dlp[default] && /opt/yt-dlp/bin/python -m compileall -q /opt/yt-dlp" \
    MALLOC_ARENA_MAX=2 \
    DENO_NO_UPDATE_CHECK=1 \
    DENO_DIR=/data/tmp/deno_cache \
    DENO_V8_FLAGS="--max-old-space-size=256"

WORKDIR /data
VOLUME ["/data"]
EXPOSE 8080

# ---- runtime-dpi (DPI bypass with zapret2) ----------------------------------
FROM base-runtime AS runtime-dpi

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        iptables \
        ipset \
        procps \
        gosu \
    && rm -rf /var/lib/apt/lists/* \
    && update-alternatives --set iptables /usr/sbin/iptables-legacy \
    && update-alternatives --set ip6tables /usr/sbin/ip6tables-legacy

COPY --from=zapret /opt/zapret2 /opt/zapret2
RUN ln -s /opt/zapret2/nfqws2 /usr/local/bin/nfqws2

COPY packaging/entrypoint-dpi.sh /entrypoint.sh
RUN chmod 755 /entrypoint.sh

ENV NFQWS2_OPT="--lua-desync=multidisorder:pos=1,midsld"

ENTRYPOINT ["/entrypoint.sh"]
CMD ["/usr/local/bin/tg-media-bot"]

# ---- runtime (default standard image) ---------------------------------------
FROM base-runtime AS runtime

USER app
ENTRYPOINT ["/usr/local/bin/tg-media-bot"]
