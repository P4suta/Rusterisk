FROM rust:1.100.0-trixie@sha256:223cc32ddca8f3ee90c436dbc94b6b5f052ea7f9653c5201e9612f08ad7d21b8 AS bundle

SHELL ["/bin/bash", "-euo", "pipefail", "-c"]

ARG TARGETARCH
ENV CARGO_BUILD_JOBS=1

RUN case "$TARGETARCH" in \
        amd64) target=x86_64-unknown-linux-gnu; digest=32e2b484d73ede0bbf64a2cf0879c4259422497d8aec4ee67d448de9ae7843d3 ;; \
        arm64) target=aarch64-unknown-linux-gnu; digest=0042d2688bf1550270def4a79c96aaeea138631818cb4ab5480120f2d51a942c ;; \
        *) exit 1 ;; \
    esac \
    && timeout 300 cargo install --locked kani-verifier@0.68.0 \
    && curl --fail --location --max-time 120 --output /tmp/kani.tar.gz \
        "https://github.com/model-checking/kani/releases/download/kani-0.68.0/kani-0.68.0-${target}.tar.gz" \
    && printf '%s  %s\n' "$digest" /tmp/kani.tar.gz | sha256sum --check --strict \
    && timeout 120 cargo kani setup --use-local-bundle /tmp/kani.tar.gz \
    && case "$(cargo kani --version)" in \
        "Kani Rust Verifier 0.68.0 (cargo plugin)"*) ;; \
        *) exit 1 ;; \
    esac \
    && rm /tmp/kani.tar.gz

FROM bundle AS compiler
SHELL ["/bin/bash", "-euo", "pipefail", "-c"]
WORKDIR /build/kani
ENV KANI_RELEASE=0d2328a93f0e0ff66132d6bfa1a7d884877cf862 \
    KANI_BACKPORT=1bf25ce33c06031755053811eabc7298ef5d5c0e
RUN git init \
    && git remote add origin https://github.com/model-checking/kani.git \
    && git fetch --depth=1 origin "$KANI_RELEASE" \
    && git checkout --detach "$KANI_RELEASE" \
    && git fetch --depth=2 origin "$KANI_BACKPORT" \
    && git diff "${KANI_BACKPORT}^" "$KANI_BACKPORT" | git apply - \
    && git submodule update --init --depth=1 charon \
    && rustup component add --toolchain nightly-2026-08-21 rustc-dev rust-src \
    && cargo +nightly-2026-08-21 build --locked --release --jobs=1 --package kani-compiler

FROM bundle
COPY --from=compiler /build/kani/target/release/kani-compiler /root/.kani/kani-0.68.0/bin/kani-compiler
LABEL org.rusterisk.verification.kani-backport="1bf25ce33c06031755053811eabc7298ef5d5c0e"
WORKDIR /tmp
