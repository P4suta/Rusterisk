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

distribution-check:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution-check

distribution-sdk-plan:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution sdk plan

distribution-sdk-build:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution sdk build

distribution-sdk-check-content candidate:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution sdk check-content --candidate {{ quote(candidate) }}

distribution-sdk-verify candidate:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution sdk verify --candidate {{ quote(candidate) }}

distribution-server-plan target:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server plan --target {{ quote(target) }}

distribution-server-build target:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server build --target {{ quote(target) }}

distribution-server-check-content candidate target:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server check-content --candidate {{ quote(candidate) }} --target {{ quote(target) }}

browser-example port="3000": browser-check
    cargo run --locked --package rusterisk-server --example browser -- "{{ port }}"

browser *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- browser {{ arguments }}

security:
    cargo run --locked --target-dir target/xtask --package xtask -- security

vendor-check:
    cargo run --locked --target-dir target/xtask --package xtask -- vendor-check

vendor-import archive:
    cargo run --locked --target-dir target/xtask --package xtask -- vendor-import --archive "{{ archive }}"

vendor-patch archive:
    cargo run --locked --target-dir target/xtask --package xtask -- vendor-patch --archive "{{ archive }}"

fmt:
    cargo fmt
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

core-proofs-build *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- core-proofs-build {{ arguments }}

core-miri *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- core-miri {{ arguments }}

core-mutation *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- core-mutation {{ arguments }}

input-fuzz *arguments="all --dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- input-fuzz {{ arguments }}

native-sanitizers *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- native-sanitizers {{ arguments }}
