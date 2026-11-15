---
date: 2026-11-16
status: proposed
---

# Verify browser calls in an owned loopback container

## Context

The [headless shell delegate](https://chromium.googlesource.com/chromium/src/+/159.0.8154.6/headless/lib/browser/headless_web_contents_impl.cc#112) inherits an [unsupported media permission handler](https://chromium.googlesource.com/chromium/src/+/159.0.8154.6/content/public/browser/web_contents_delegate.cc#301).
The [Playwright image](https://playwright.dev/docs/docker) includes matching browsers and system dependencies.
[Secure contexts](https://www.w3.org/TR/secure-contexts/#is-origin-trustworthy) constrain microphone access.

## Decision

Pin Playwright 1.65.1's image digest, Rust, and Node.
Select the [Chromium channel](https://playwright.dev/docs/browsers#chromium-new-headless-mode) for media permissions.
Run the source-bound server, example listener, and browsers inside one owned container.
Keep signaling and media on loopback without host gateway routing.
Enable Chromium's [loopback peer interfaces](https://chromium.googlesource.com/chromium/src/+/159.0.8154.6/content/public/common/content_switches.cc#21) for offline media tests.
Bound browser phases and [`BrowserServer.kill()`](https://playwright.dev/docs/api/class-browserserver#browser-server-kill).
Use one worker, no retries, two processors, and two gibibytes of memory.
Give Chromium bounded shared memory instead of the host's interprocess namespace.
Serve the fixture and reject external page requests.

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
