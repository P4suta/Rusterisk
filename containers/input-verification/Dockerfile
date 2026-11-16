FROM rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6

SHELL ["/bin/bash", "-euo", "pipefail", "-c"]

ARG TARGETARCH
ENV CARGO_BUILD_JOBS=1

RUN timeout 180 apt-get update \
    && timeout 300 apt-get install --yes --no-install-recommends clang=1:14.0-55.7~deb12u1 \
    && rm -rf /var/lib/apt/lists/*

RUN case "$TARGETARCH" in \
        amd64) architecture=x86_64; digest=ed85135c5a5135b45e50423cb406dea04ca3338da0b8ef3c9ede963559680be0 ;; \
        arm64) architecture=aarch64; digest=301d5063ab259577c1b57ce5490b1aab9ea5c56dc4c822f73499553dab94925f ;; \
        *) exit 1 ;; \
    esac \
    && curl --fail --location --proto '=https' --tlsv1.2 --max-time 120 \
        --output /tmp/cmake.tar.gz \
        "https://github.com/Kitware/CMake/releases/download/v4.4.5/cmake-4.4.5-linux-${architecture}.tar.gz" \
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
    && test "$(cmake --version | head -n 1)" = 'cmake version 4.4.5' \
    && rustc +nightly-2026-08-21 -vV | grep --fixed-strings --line-regexp 'commit-hash: 8925ea358a0f265ca61026aadc7ecc506c545cbe'

RUN timeout 180 apt-get update \
    && timeout 300 apt-get install --yes --no-install-recommends \
        libclang-rt-14-dev=1:14.0.6-12 llvm-14=1:14.0.6-12 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /tmp
