---
date: 2026-11-16
status: proposed
---

# Verify browser calls in an owned loopback container

## Context

Browser calls require an actual browser, signaling, and received-audio checks.
The [Playwright image](https://playwright.dev/docs/docker) includes matching browsers and system dependencies.
[Secure contexts](https://www.w3.org/TR/secure-contexts/#is-origin-trustworthy) constrain microphone access.

## Decision

Pin Playwright 1.65.1's public image by digest and add the pinned Rust and Node toolchains.
Run the source-bound server, example listener, and browsers inside one owned container.
Keep signaling and media on loopback without host gateway routing.
Use one test worker, no retries, finite deadlines, two processors, and two gibibytes of memory.
Give Chromium bounded shared memory instead of the host's interprocess namespace.
Serve only the maintained fixture and reject external page requests.

## Alternatives

Host browsers need machine-specific installation and weaker process resource isolation.
A remote browser adds gateway routing and trust configuration to local media tests.
Reusing the input-verification image would combine unrelated large tool inventories.

## Consequences

The browser image costs storage and a separate bounded build.
Reconsider compilation inside the image when a verified distribution candidate supplies the test server.

## Confirmation

Compare received tones and stored recording samples in both directions.
Exercise microphone denial, cancellation, disconnect, layout, and owned resource cleanup.
