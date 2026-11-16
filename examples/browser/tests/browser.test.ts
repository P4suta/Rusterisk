import assert from "node:assert/strict";
import test from "node:test";
import {
	automationFailure,
	checkBrowser,
	nativeBrowserOptions,
} from "./browser.ts";

await test("native media uses the full pinned Chromium channel and keeps per-browser capture and permission decisions separate", (): void => {
	for (const file of ["/owned/alice.wav", "/owned/bob.wav"]) {
		const options = nativeBrowserOptions(file);
		assert.equal(options.channel, "chromium");
		assert.equal(options.headless, true);
		assert.ok(
			options.args?.includes(`--use-file-for-fake-audio-capture=${file}`),
		);
		assert.ok(
			options.args?.every(
				(argument: string): boolean =>
					!argument.startsWith("--use-fake-ui-for-media-stream") &&
					!argument.startsWith("--auto-accept-camera-and-microphone-capture") &&
					!argument.startsWith("--disable-web-security"),
			),
		);
	}
	checkBrowser({ version: (): string => "159.0.8154.6" });
	for (const version of ["", "158.0.0.0", "159.0.8154.7"])
		assert.throws((): void => {
			checkBrowser({ version: (): string => version });
		});
});

await test("automation failures retain their classification without exposing the private browser capability", (): void => {
	const capability = "ws://127.0.0.1:32000/private-automation-capability";
	const native = new Error(`Connection to ${capability} did not finish.`, {
		cause: new Error(capability),
	});
	native.name = "TimeoutError";
	const failure = automationFailure(native);
	assert.equal(
		failure.message,
		"Owned Chromium automation connection failed: TimeoutError.",
	);
	assert.equal(failure.cause, undefined);
	assert.ok(!failure.stack?.includes(capability));
	assert.equal(
		automationFailure(undefined).message,
		"Owned Chromium automation connection failed: Unknown.",
	);
});
