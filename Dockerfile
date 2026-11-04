# The Meridian proxy as a container image.
#
#   docker build -t meridian .
#   docker run --rm -p 25565:25565 -v /srv/meridian:/etc/meridian:ro meridian
#
# The configuration is a volume: the image reads /etc/meridian/meridian.toml, and a relative
# path in that file (a sidecar secret) is taken from that folder. The proxy runs as the
# unprivileged user of the base image. `docker stop` sends SIGTERM, which stops the proxy
# cleanly. See docs/OPERATING.md, section 5.7.

# The Rust version is an argument. The sources need Rust 1.85 or later.
ARG RUST_VERSION=1.90

FROM rust:${RUST_VERSION}-slim-bookworm AS build
WORKDIR /src
COPY . .
# --locked builds exactly what Cargo.lock says. The Minecraft protocol crate comes from
# Horizon's repository on GitHub, so the build needs network access.
RUN cargo build --release --locked -p meridian-proxy --bin meridian \
    --config 'profile.release.strip="symbols"'

# Debian 12 like the build stage, so that the libraries the binary links to are the same.
# This image holds the C library, the certificates and an unprivileged user (65532), and no
# shell and no package manager.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/meridian /usr/local/bin/meridian
COPY --from=build /src/meridian.example.toml /usr/share/meridian/meridian.example.toml
WORKDIR /etc/meridian
VOLUME /etc/meridian
EXPOSE 25565
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/meridian"]
CMD ["/etc/meridian/meridian.toml"]
