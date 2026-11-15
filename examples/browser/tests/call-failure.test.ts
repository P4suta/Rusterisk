import assert from "node:assert/strict";
import test from "node:test";
import { releaseFailedCall } from "../src/call-failure.ts";

await test("failed outgoing setup always releases ownership and retains setup and cleanup failures", async (): Promise<void> => {
	for (const disposeFails of [true, false]) {
		for (const releaseFails of [false, true]) {
			const primary = new Error("Outgoing setup failed.");
			const disposal = new Error("Session disposal failed.");
			const ownership = new Error("Ownership release failed.");
			const order: string[] = [];
			let owned = true;
			await assert.rejects(
				releaseFailedCall(
					primary,
					(): Promise<void> => {
						order.push("dispose");
						return disposeFails ? Promise.reject(disposal) : Promise.resolve();
					},
					(): void => {
						order.push("release");
						owned = false;
						if (releaseFails) throw ownership;
					},
				),
				(error: unknown): boolean => {
					assert.deepEqual(order, ["dispose", "release"]);
					assert.equal(owned, false);
					assert.ok(error instanceof Error);
					assert.equal(error.cause, primary);
					if (disposeFails || releaseFails) {
						assert.ok(error instanceof AggregateError);
						assert.deepEqual(error.errors, [
							primary,
							...(disposeFails ? [disposal] : []),
							...(releaseFails ? [ownership] : []),
						]);
					} else {
						assert.equal(error.message, "The outgoing call failed.");
					}
					return true;
				},
			);
			assert.deepEqual(order, ["dispose", "release"]);
			assert.equal(owned, false);
		}
	}
});
