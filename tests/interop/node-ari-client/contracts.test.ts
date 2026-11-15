import assert from "node:assert/strict";
import test from "node:test";
import { application, endpoint, target } from "./contracts.ts";

await test("endpoint snapshots validate remote fields and keep only contract data", (): void => {
	assert.deepEqual(
		endpoint({
			technology: "PJSIP",
			resource: "phone",
			state: "online",
			channel_ids: ["call"],
			extra: "ignored",
		}),
		{
			technology: "PJSIP",
			resource: "phone",
			state: "online",
			channel_ids: ["call"],
		},
	);
	assert.equal(
		endpoint({ technology: "PJSIP", resource: "phone", channel_ids: [] }).state,
		undefined,
	);
	for (const invalid of [
		null,
		[],
		{
			technology: "PJSIP",
			resource: "phone",
			state: "connected",
			channel_ids: [],
		},
		{ technology: "PJSIP", resource: "phone", channel_ids: [1] },
		{
			technology: "PJSIP",
			resource: "phone",
			channel_ids: Array.from({ length: 65 }, (): string => "call"),
		},
	]) {
		assert.throws((): void => {
			endpoint(invalid);
		});
	}
});

await test("comparison targets require an explicit local management address", (): void => {
	assert.equal(target("http://127.0.0.1:8088"), "http://127.0.0.1:8088");
	for (const invalid of [
		undefined,
		"http://example.com",
		"http://127.0.0.1.evil.test",
		"http://user:secret@127.0.0.1",
		"http://127.0.0.1/ari",
		"http://127.0.0.1?token=secret",
	]) {
		assert.throws((): void => {
			target(invalid);
		});
	}
});

await test("app snapshots validate membership arrays from the existing client", (): void => {
	assert.deepEqual(
		application({
			name: "compat-node",
			channel_ids: [],
			bridge_ids: [],
			endpoint_ids: [],
			device_names: [],
		}),
		{
			name: "compat-node",
			channel_ids: [],
			bridge_ids: [],
			endpoint_ids: [],
			device_names: [],
		},
	);
	for (const invalid of [
		null,
		{},
		{
			name: "compat-node",
			channel_ids: [1],
			bridge_ids: [],
			endpoint_ids: [],
			device_names: [],
		},
	]) {
		assert.throws((): void => {
			application(invalid);
		});
	}
});
