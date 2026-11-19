import assert from "node:assert/strict";
import test from "node:test";
import { type CallChannels, usingCallChannels } from "./call-channels.ts";

interface Fixture {
	readonly channels: CallChannels;
	readonly active: Set<string>;
	readonly attempts: string[];
}

function fixture(fault?: { id: string; reason: Error }): Fixture {
	const active = new Set<string>();
	const attempts: string[] = [];
	return {
		active,
		attempts,
		channels: {
			list: (): Promise<readonly string[]> => Promise.resolve([...active]),
			hangup: (id: string): Promise<void> => {
				attempts.push(id);
				if (id === fault?.id) return Promise.reject(fault.reason);
				assert.equal(active.delete(id), true);
				return Promise.resolve();
			},
			requireEmpty: (): Promise<void> => {
				assert.equal(active.size, 0);
				return Promise.resolve();
			},
		},
	};
}

await test("a call case cannot start on a server with an existing channel", async (): Promise<void> => {
	const { active, attempts, channels } = fixture();
	active.add("existing");
	let started = false;
	await assert.rejects(
		usingCallChannels(channels, (): Promise<void> => {
			started = true;
			return Promise.resolve();
		}),
		/active channels before the case/,
	);
	assert.equal(started, false);
	assert.deepEqual(attempts, []);
	assert.deepEqual([...active], ["existing"]);
});

await test("a failed call case releases both channels and preserves its error", async (): Promise<void> => {
	const { active, attempts, channels } = fixture();
	const reason = new Error("The incoming call did not ring.");
	await assert.rejects(
		usingCallChannels(channels, (): Promise<void> => {
			active.add("alice");
			active.add("bob");
			return Promise.reject(reason);
		}),
		(error: unknown): boolean => error === reason,
	);
	assert.deepEqual(attempts, ["alice", "bob"]);
	assert.equal(active.size, 0);
});

await test("a failed hangup keeps both errors and still releases the other channel", async (): Promise<void> => {
	const failed = new Error("The first hangup failed.");
	const { active, attempts, channels } = fixture({
		id: "alice",
		reason: failed,
	});
	const reason = new Error("The incoming call did not ring.");
	await assert.rejects(
		usingCallChannels(channels, (): Promise<void> => {
			active.add("alice");
			active.add("bob");
			return Promise.reject(reason);
		}),
		(error: unknown): boolean => {
			assert.ok(error instanceof AggregateError);
			assert.equal(error.cause, reason);
			assert.equal(error.errors[0], reason);
			const cleanup: unknown = error.errors[1];
			assert.ok(cleanup instanceof AggregateError);
			assert.deepEqual(cleanup.errors, [failed]);
			return true;
		},
	);
	assert.deepEqual(attempts, ["alice", "bob"]);
	assert.deepEqual([...active], ["alice"]);
});
