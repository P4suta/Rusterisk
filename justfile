set shell := ["sh", "-cu"]

default:
    @just --list

check:
    cargo run --locked --target-dir target/xtask --package xtask -- check

policy:
    cargo run --locked --target-dir target/xtask --package xtask -- policy

core-msrv:
    cargo run --locked --target-dir target/xtask --package xtask -- core-msrv

[positional-arguments]
server-footprint +args:
    cargo run --locked --target-dir target/xtask --package xtask -- server-footprint "$@"

node-check:
    cargo run --locked --target-dir target/xtask --package xtask -- node-check

browser-check:
    cargo run --locked --target-dir target/xtask --package xtask -- browser-check

distribution-check:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution-check

distribution-cli-check:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution cli verify

distribution-sdk-plan:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution sdk plan

distribution-sdk-build:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution sdk build

distribution-sdk-check-content candidate:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution sdk check-content --candidate {{ quote(candidate) }}

distribution-sdk-verify candidate:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution sdk verify --candidate {{ quote(candidate) }}

distribution-server-plan target jobs="2" build_timeout_seconds="900":
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server plan --target {{ quote(target) }} --jobs {{ quote(jobs) }} --build-timeout-seconds {{ quote(build_timeout_seconds) }}

distribution-server-build target jobs="2" build_timeout_seconds="900":
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server build --target {{ quote(target) }} --jobs {{ quote(jobs) }} --build-timeout-seconds {{ quote(build_timeout_seconds) }}

distribution-server-plan-container target image:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server plan-container --target {{ quote(target) }} --image {{ quote(image) }}

distribution-server-build-container target image:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server build-container --target {{ quote(target) }} --image {{ quote(image) }}

distribution-server-export-oci candidate target:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server export-oci --candidate {{ quote(candidate) }} --target {{ quote(target) }}

distribution-server-verify-oci candidate target archive sdk_candidate:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server verify-oci --candidate {{ quote(candidate) }} --target {{ quote(target) }} --archive {{ quote(archive) }} --sdk-candidate {{ quote(sdk_candidate) }}

distribution-server-check-content candidate target:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server check-content --candidate {{ quote(candidate) }} --target {{ quote(target) }}

distribution-server-check-startup candidate:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server check-startup --candidate {{ quote(candidate) }}

distribution-server-check-startup-target candidate target:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server check-startup --candidate {{ quote(candidate) }} --target {{ quote(target) }}

distribution-server-check-startup-rosetta candidate:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server check-startup --candidate {{ quote(candidate) }} --target x86_64-apple-darwin --rosetta

distribution-server-verify candidate sdk_candidate:
    cargo run --locked --target-dir target/xtask --package xtask -- distribution server verify --candidate {{ quote(candidate) }} --sdk-candidate {{ quote(sdk_candidate) }}

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

release-check *arguments="--dry-run":
    cargo run --locked --target-dir target/xtask --package xtask -- release-check {{ arguments }}

[positional-arguments]
release-publication +args:
    cargo run --locked --target-dir target/xtask --package xtask -- release-publication "$@"

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
