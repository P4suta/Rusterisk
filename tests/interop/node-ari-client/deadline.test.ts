import assert from "node:assert/strict";
import test from "node:test";
import { deadline } from "./deadline.ts";

await test("unfinished client operations report their stage within a deadline", async (): Promise<void> => {
	const pending = new Promise<string>((): void => {});
	await assert.rejects(deadline("discovery", pending, 10), {
		message: "discovery exceeded its deadline",
	});
	assert.equal(
		await deadline("completed", Promise.resolve("ready"), 100),
		"ready",
	);
	await assert.rejects(
		deadline("rejected", Promise.reject(new Error("original failure")), 100),
		{ message: "original failure" },
	);
	await assert.rejects(deadline("invalid", Promise.resolve("ready"), 0), {
		message: "invalid client operation budget",
	});
});
