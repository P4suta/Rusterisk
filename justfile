set shell := ["sh", "-cu"]

default:
    @just --list

check:
    cargo run --locked --target-dir target/xtask --package xtask -- check

policy:
    cargo run --locked --target-dir target/xtask --package xtask -- policy

core-msrv:
    cargo run --locked --target-dir target/xtask --package xtask -- core-msrv

security:
    cargo run --locked --target-dir target/xtask --package xtask -- security

fmt:
    cargo fmt --all

reference-build *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- reference-build {{ arguments }}

interop *arguments="":
    cargo run --locked --target-dir target/xtask --package xtask -- interop {{ arguments }}
