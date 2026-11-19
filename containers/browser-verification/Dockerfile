FROM rust:1.100.0-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea AS rust
FROM mcr.microsoft.com/playwright:v1.65.1-noble@sha256:1507cbfc550a63a6b7396791ca4f00df17beb56342d2447b3f106c306f525579

SHELL ["/bin/bash", "-euo", "pipefail", "-c"]

ARG TARGETARCH
COPY --from=rust /usr/local/rustup /usr/local/rustup
COPY --from=rust /usr/local/cargo /usr/local/cargo

ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=/usr/local/cargo
ENV CARGO_BUILD_JOBS=1

RUN timeout 180 apt-get update \
    && timeout 300 apt-get install --yes --no-install-recommends \
        build-essential=12.10ubuntu1 \
        gcc=4:13.2.0-7ubuntu1 \
        g++=4:13.2.0-7ubuntu1 \
    && rm -rf /var/lib/apt/lists/*

RUN case "$TARGETARCH" in \
        amd64) node_arch=x64; node_digest=5c063e46bab0fbc1c17cc445d7ff129cbdcd0471be6ed83b6ab864f8143bc864; cmake_arch=x86_64; cmake_digest=ed85135c5a5135b45e50423cb406dea04ca3338da0b8ef3c9ede963559680be0 ;; \
        arm64) node_arch=arm64; node_digest=89e1853138db51e8b82d3e98ba9c0d3f2956a28e5e55ea1b7e12d25aae92d5dd; cmake_arch=aarch64; cmake_digest=301d5063ab259577c1b57ce5490b1aab9ea5c56dc4c822f73499553dab94925f ;; \
        *) exit 1 ;; \
    esac \
    && curl --fail --location --proto '=https' --tlsv1.2 --max-time 120 \
        --output /tmp/node.tar.xz \
        "https://nodejs.org/download/release/v26.13.1/node-v26.13.1-linux-${node_arch}.tar.xz" \
    && printf '%s  %s\n' "$node_digest" /tmp/node.tar.xz | sha256sum --check --strict \
    && mkdir /opt/node \
    && tar --extract --xz --file /tmp/node.tar.xz --strip-components=1 --directory /opt/node \
    && curl --fail --location --proto '=https' --tlsv1.2 --max-time 120 \
        --output /tmp/cmake.tar.gz \
        "https://github.com/Kitware/CMake/releases/download/v4.4.5/cmake-4.4.5-linux-${cmake_arch}.tar.gz" \
    && printf '%s  %s\n' "$cmake_digest" /tmp/cmake.tar.gz | sha256sum --check --strict \
    && mkdir /opt/cmake \
    && tar --extract --gzip --file /tmp/cmake.tar.gz --strip-components=1 --directory /opt/cmake \
    && rm /tmp/node.tar.xz /tmp/cmake.tar.gz

ENV PATH="/opt/node/bin:/opt/cmake/bin:/usr/local/cargo/bin:${PATH}"

RUN test "$(node --version)" = 'v26.13.1' \
    && test "$(rustc --version | cut --delimiter=' ' --fields=2)" = '1.100.0' \
    && test "$(cmake --version | head -n 1)" = 'cmake version 4.4.5'

WORKDIR /tmp
