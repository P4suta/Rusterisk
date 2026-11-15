import assert from "node:assert/strict";
import test from "node:test";
import { SipPhone } from "../src/sip.ts";

await test("the signaling adapter refuses call media without a registered connection and leaves idle hangup harmless", async (): Promise<void> => {
	let cleared = 0;
	const phone = new SipPhone({
		attach(): Promise<void> {
			return Promise.reject(
				new Error("An idle adapter must not attach media."),
			);
		},
		clear(): void {
			cleared += 1;
		},
	});
	const capture = {
		get stream(): MediaStream {
			throw new Error("An idle adapter must not inspect capture tracks.");
		},
		stop(): void {
			throw new Error("An idle adapter must not take capture ownership.");
		},
	};
	await assert.rejects(
		phone.call("sip:100@localhost", capture, {
			answered(): void {},
			ended(): void {},
		}),
	);
	await assert.rejects(phone.answer(capture));
	await phone.hangup();
	assert.equal(cleared, 0);
});
