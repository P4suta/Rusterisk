import assert from "node:assert/strict";
import test from "node:test";
import { captureCheckpoint } from "./capture-checkpoint.ts";
import type { CaptureState } from "./capture-observer.ts";

await test("native capture failure reports bounded state and preserves the failed milestone", async (): Promise<void> => {
	const state: CaptureState = {
		attempts: 1,
		streams: 0,
		waiting: false,
		failure: "NotReadableError",
		secure: true,
		call: "No active call",
		problem: "The call could not start.",
	};
	const primary = new Error("The expected media milestone was absent.");
	let reads = 0;
	const read = (): Promise<CaptureState> => {
		reads += 1;
		return Promise.resolve(state);
	};
	await captureCheckpoint(read, (): Promise<void> => Promise.resolve());
	assert.equal(reads, 0);
	const fail = (): Promise<void> => Promise.reject(primary);
	await assert.rejects(
		captureCheckpoint(read, fail),
		(error: unknown): boolean => {
			assert.ok(error instanceof Error);
			assert.equal(error.cause, primary);
			for (const value of [
				'"attempts":1',
				'"streams":0',
				'"secure":true',
				"NotReadableError",
				"No active call",
				"The call could not start.",
			])
				assert.ok(error.message.includes(value), error.message);
			return true;
		},
	);
	assert.equal(reads, 1);
	await assert.rejects(
		captureCheckpoint(
			(): Promise<CaptureState> =>
				Promise.reject(new Error("Unfiltered native failure text.")),
			fail,
		),
		(error: unknown): boolean => {
			assert.ok(error instanceof Error);
			assert.equal(error.cause, primary);
			assert.match(error.message, /"unavailable":"Error"/u);
			assert.ok(!error.message.includes("Unfiltered native failure text"));
			return true;
		},
	);
});

await test("unresponsive capture diagnostics cannot hide the original failure", async (): Promise<void> => {
	const primary = new Error("The expected media milestone was absent.");
	await assert.rejects(
		captureCheckpoint(
			(): Promise<CaptureState> => new Promise<CaptureState>((): void => {}),
			(): Promise<void> => Promise.reject(primary),
			0,
		),
		(error: unknown): boolean => {
			assert.ok(error instanceof Error);
			assert.equal(error.cause, primary);
			assert.match(error.message, /"unavailable":"DeadlineError"/u);
			return true;
		},
	);
});
