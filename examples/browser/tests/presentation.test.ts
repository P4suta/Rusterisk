import assert from "node:assert/strict";
import test from "node:test";
import type { PhoneState } from "../src/phone.ts";
import { presentation } from "../src/presentation.ts";

await test("call controls expose connection, incoming answers, and cancellation without accepting duplicate actions", (): void => {
	const idle: PhoneState = {
		connection: "disconnected",
		call: "idle",
		caller: "",
		error: "",
	};
	assert.deepEqual(presentation(idle), {
		connection: "Disconnected",
		call: "No active call",
		account: true,
		destination: false,
		connect: true,
		disconnect: false,
		dial: false,
		answer: false,
		hangup: false,
	});
	const ready = presentation({ ...idle, connection: "registered" });
	assert.equal(ready.dial, true);
	assert.equal(ready.account, false);
	assert.equal(ready.disconnect, true);
	const incoming = presentation({
		...idle,
		connection: "registered",
		call: "incoming",
		caller: "Alice",
	});
	assert.equal(incoming.answer, true);
	assert.equal(incoming.dial, false);
	assert.equal(incoming.hangup, true);
	assert.equal(incoming.call, "Incoming call from Alice");
	for (const call of [
		"call-microphone",
		"answer-microphone",
		"calling",
		"answering",
		"active",
		"ending",
	] satisfies readonly PhoneState["call"][]) {
		const view = presentation({ ...idle, connection: "registered", call });
		assert.equal(view.dial, false);
		assert.equal(view.answer, false);
		assert.equal(view.hangup, call !== "ending");
	}
});
