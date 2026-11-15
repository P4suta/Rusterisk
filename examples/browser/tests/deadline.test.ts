import assert from "node:assert/strict";
import test from "node:test";
import { within } from "./deadline.ts";

await test("an unfinished native operation rejects at its named deadline", {
	timeout: 1000,
}, async (): Promise<void> => {
	await assert.rejects(
		within("Capture diagnostics", new Promise<never>((): void => {}), 0),
		(error: unknown): boolean => {
			assert.ok(error instanceof Error);
			assert.equal(error.name, "DeadlineError");
			assert.equal(error.message, "Capture diagnostics exceeded its deadline.");
			return true;
		},
	);
});

await test("completed native operations retain their values and original failures", async (): Promise<void> => {
	assert.equal(await within("Completed capture", Promise.resolve(7), 1000), 7);
	const primary = new Error("The expected milestone was absent.");
	await assert.rejects(
		within("Failed capture", Promise.reject(primary), 1000),
		(error: unknown): boolean => error === primary,
	);
});
