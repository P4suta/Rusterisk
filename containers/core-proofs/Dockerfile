FROM rust:1.99.0-bookworm@sha256:114c7a4425406451c2866b6aafe69fe29b1b298832db1277d411ac73c82d04d6

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

WORKDIR /tmp
