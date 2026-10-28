FROM rust:1.89-bookworm AS backend-build
WORKDIR /src
RUN apt-get update \
    && apt-get install --no-install-recommends -y pkg-config libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libgstreamer-plugins-bad1.0-dev \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked --features relay-server/gstreamer --bin relay-server

FROM debian:bookworm-slim AS runtime
ENV DEBIAN_FRONTEND=noninteractive \
    DISPLAY=:99 \
    BROWSER_RELAY_BIND=0.0.0.0:8080 \
    BROWSER_RELAY_CHROMIUM=/usr/bin/chromium
RUN apt-get update \
    && apt-get install --no-install-recommends -y chromium xvfb ca-certificates procps gstreamer1.0-tools gstreamer1.0-nice gstreamer1.0-x gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash browserrelay \
    && mkdir -p /tmp/browser-relay-profile \
    && chown -R browserrelay:browserrelay /tmp/browser-relay-profile
COPY --from=backend-build /src/target/release/relay-server /usr/local/bin/relay-server
COPY docker/entrypoint.sh /usr/local/bin/browserrelay-entrypoint
RUN chmod 0755 /usr/local/bin/browserrelay-entrypoint /usr/local/bin/relay-server
USER browserrelay
WORKDIR /home/browserrelay
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/browserrelay-entrypoint"]
