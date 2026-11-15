import assert from "node:assert/strict";
import test from "node:test";
import { type AccountFields, connectionOptions } from "../src/config.ts";

const account = {
	server: "ws://127.0.0.1:8088/ws",
	address: "sip:alice@localhost",
	username: "alice",
	password: "pass word",
	ice: "",
};

await test("connection options admit loopback development and secure remote signaling without changing credentials", (): void => {
	assert.deepEqual(connectionOptions(account), {
		server: account.server,
		address: account.address,
		username: account.username,
		password: account.password,
		iceServers: [],
	});
	const ice =
		'[{"urls":["stun:stun.example.test","turns:turn.example.test:5349"],"username":"alice","credential":"temporary"}]';
	assert.equal(
		connectionOptions({
			...account,
			server: "wss://calls.example.test/ws",
			ice,
		}).iceServers.length,
		1,
	);
});

await test("account validation identifies the rejected field without exposing private input or parser failures", (): void => {
	const invalid: readonly (readonly [keyof AccountFields, string, string])[] = [
		[
			"server",
			"wss://alice:private-fixture-password@example.test/ws",
			"WebSocket server URL",
		],
		[
			"address",
			"sip:alice:private-fixture-password@example.test",
			"Account SIP address",
		],
		["username", "line\nbreak", "Authentication username"],
		["password", "", "Password"],
		["ice", '{"credential":"private-fixture-password"', "ICE servers"],
	];
	for (const [field, value, label] of invalid)
		assert.throws(
			(): void => {
				connectionOptions({ ...account, [field]: value });
			},
			(error: unknown): boolean => {
				assert.ok(error instanceof Error);
				assert.ok("field" in error);
				assert.equal(error.field, field);
				assert.ok(error.message.startsWith(`${label}:`));
				assert.ok(!error.message.includes("private-fixture-password"));
				assert.equal(error.cause, undefined);
				return true;
			},
		);
});

await test("connection options reject insecure remote URLs, credentials in URLs, invalid addresses, and unbounded ICE configuration", (): void => {
	for (const server of [
		"ws://example.test/ws",
		"https://example.test/ws",
		"wss://alice:secret@example.test/ws",
		"wss://example.test/ws#secret",
		"wss://example.test/ws?secret=1",
	])
		assert.throws((): void => {
			connectionOptions({ ...account, server });
		});
	for (const address of [
		"not a URI",
		"sip:",
		"sip:alice:secret@example.test",
		"sip:alice@example.test\r\nInjected: value",
	])
		assert.throws((): void => {
			connectionOptions({ ...account, address });
		});
	for (const ice of [
		"{}",
		"[1]",
		'[{"urls":"https://example.test"}]',
		'[{"urls":"turn:example.test","credential":1}]',
		JSON.stringify(
			Array.from({ length: 9 }, (): object => ({ urls: "stun:example.test" })),
		),
		"x".repeat(8193),
	])
		assert.throws((): void => {
			connectionOptions({ ...account, ice });
		});
});
