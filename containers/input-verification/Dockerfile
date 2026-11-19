FROM rust:1.100.0-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea

SHELL ["/bin/bash", "-euo", "pipefail", "-c"]

ARG TARGETARCH
ENV CARGO_BUILD_JOBS=1

RUN timeout 180 apt-get update \
    && timeout 300 apt-get install --yes --no-install-recommends clang=1:14.0-55.7~deb12u1 \
    && rm -rf /var/lib/apt/lists/*

RUN case "$TARGETARCH" in \
        amd64) architecture=x86_64; digest=81f0d5b0960fb6adc8197030ffc827fba937640d51e73e39e0e9e40d88c66f6f ;; \
        arm64) architecture=aarch64; digest=0c8447e7bba9701467b5ac6155d157aa9e309e71af4981f78bd4b9176107f553 ;; \
        *) exit 1 ;; \
    esac \
    && curl --fail --location --proto '=https' --tlsv1.2 --max-time 120 \
        --output /tmp/cmake.tar.gz \
        "https://github.com/Kitware/CMake/releases/download/v4.5.0/cmake-4.5.0-linux-${architecture}.tar.gz" \
    && printf '%s  %s\n' "$digest" /tmp/cmake.tar.gz | sha256sum --check --strict \
    && mkdir /opt/cmake \
    && tar --extract --gzip --file /tmp/cmake.tar.gz --strip-components=1 --directory /opt/cmake \
    && rm /tmp/cmake.tar.gz

ENV PATH="/opt/cmake/bin:${PATH}"
ENV CC=clang CXX=clang++

RUN timeout 600 rustup toolchain install nightly-2026-08-21 --profile minimal --component miri,rust-src \
    && timeout 300 cargo install --locked cargo-fuzz@0.13.2 \
    && timeout 600 cargo install --locked cargo-mutants@27.1.0 \
    && test "$(cargo fuzz --version)" = 'cargo-fuzz 0.13.2' \
    && test "$(cargo mutants --version)" = 'cargo-mutants 27.1.0' \
    && test "$(cmake --version | head -n 1)" = 'cmake version 4.5.0' \
    && rustc +nightly-2026-08-21 -vV | grep --fixed-strings --line-regexp 'commit-hash: 8925ea358a0f265ca61026aadc7ecc506c545cbe'

RUN timeout 180 apt-get update \
    && timeout 300 apt-get install --yes --no-install-recommends \
        libclang-rt-14-dev=1:14.0.6-12 llvm-14=1:14.0.6-12 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /tmp
