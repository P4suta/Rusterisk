set shell := ["sh", "-cu"]

default:
    @just --list

check:
    cargo run --locked --target-dir target/xtask --package xtask -- check

policy:
    cargo run --locked --target-dir target/xtask --package xtask -- policy

security:
    cargo run --locked --target-dir target/xtask --package xtask -- security

fmt:
    cargo fmt --all
