FROM ubuntu:24.04

ARG RUST_TOOLCHAIN=1.88.0

ENV DEBIAN_FRONTEND=noninteractive
ENV RUSTUP_HOME=/opt/rustup
ENV PATH=/opt/cargo/bin:${PATH}

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    bash \
    build-essential \
    ca-certificates \
    curl \
    git \
    pkg-config \
    python3 \
    xz-utils \
  && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://sh.rustup.rs -o /tmp/rustup-init.sh \
  && chmod +x /tmp/rustup-init.sh \
  && CARGO_HOME=/opt/cargo /tmp/rustup-init.sh -y \
    --no-modify-path \
    --profile minimal \
    --default-toolchain "${RUST_TOOLCHAIN}" \
  && rm /tmp/rustup-init.sh \
  && chmod -R a+rx /opt/cargo /opt/rustup

ENV CARGO_HOME=/tmp/cargo-home
