import assert from "node:assert/strict";
import test from "node:test";
import { incomingCall } from "./incoming-call.ts";

await test("uses the selected server endpoint for the incoming call", (): void => {
	for (const [server, endpoint] of [
		[undefined, "PJSIP/bob-endpoint"],
		["rusterisk", "PJSIP/bob-endpoint"],
		["asterisk", "PJSIP/bob"],
	] as const) {
		const request = new URL(
			incomingCall(server, 60000),
			"http://127.0.0.1/ari/",
		);
		assert.equal(request.pathname, "/ari/channels");
		assert.equal(request.searchParams.get("endpoint"), endpoint);
		assert.equal(request.searchParams.get("app"), "calls");
		assert.equal(request.searchParams.get("channelId"), "browser-incoming");
		assert.equal(request.searchParams.get("callerId"), "100");
		assert.equal(request.searchParams.get("timeout"), "60");
	}
});

await test("rejects an unknown browser verification server", (): void => {
	assert.throws(
		() => incomingCall("unknown", 60000),
		/browser verification server/,
	);
});

await test("rounds up an incoming call budget expressed in milliseconds", (): void => {
	const request = new URL(incomingCall("asterisk", 60501), "http://127.0.0.1/");
	assert.equal(request.searchParams.get("timeout"), "61");
});

await test("rejects incoming call budgets outside the common timeout range", (): void => {
	for (const milliseconds of [
		0,
		-1,
		Number.NaN,
		Number.POSITIVE_INFINITY,
		3600001,
	])
		assert.throws(
			() => incomingCall("rusterisk", milliseconds),
			/incoming call timeout/,
		);
});
