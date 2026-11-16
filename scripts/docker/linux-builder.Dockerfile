# Builder image for portable Linux release artifacts.
#
# Ubuntu 22.04 keeps the glibc baseline at 2.35, so the tarball, .deb and
# AppImage run on Ubuntu 22.04+ and Debian 12+ instead of only on very recent
# distributions.
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        dpkg-dev \
        fakeroot \
        file \
        gzip \
        pkg-config \
        xz-utils \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain 1.95.0 \
    && cargo --version \
    && rustc --version

WORKDIR /src
