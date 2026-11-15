set shell := ["sh", "-cu"]

default:
    @just --list

check:
    cargo run --locked --target-dir target/xtask --package xtask -- check

policy:
    cargo run --locked --target-dir target/xtask --package xtask -- policy

core-msrv:
    cargo run --locked --target-dir target/xtask --package xtask -- core-msrv

node-check:
    cargo run --locked --target-dir target/xtask --package xtask -- node-check

browser-check:
    cargo run --locked --target-dir target/xtask --package xtask -- browser-check

browser-example port="3000": browser-check
    cargo run --locked --package rusterisk-server --example browser -- "{{ port }}"

browser *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- browser {{ arguments }}

security:
    cargo run --locked --target-dir target/xtask --package xtask -- security

fmt:
    cargo fmt --all
    cargo fmt --manifest-path xtask/fuzz/Cargo.toml

reference-build *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- reference-build {{ arguments }}

verification-build *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- verification-build {{ arguments }}

browser-build *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- browser-build {{ arguments }}

interop *arguments="":
    cargo run --locked --target-dir target/xtask --package xtask -- interop {{ arguments }}

core-proofs *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- core-proofs {{ arguments }}

core-miri *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- core-miri {{ arguments }}

core-mutation *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- core-mutation {{ arguments }}

input-fuzz *arguments="all --dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- input-fuzz {{ arguments }}
