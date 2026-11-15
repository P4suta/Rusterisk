import assert from "node:assert/strict";
import type { CallEvents } from "../src/phone.ts";
import { SipPhone } from "../src/sip.ts";

const server = process.env.RUSTERISK_BROWSER_SIGNALING;
assert.ok(server);
const events: CallEvents = {
	incoming: (): undefined => undefined,
	disconnected: (): void => {},
};
const phone = new SipPhone({
	attach(): Promise<void> {
		return Promise.reject(new Error("Registration must not request media."));
	},
	clear(): void {},
});
const account = {
	server,
	address: "sip:phone@localhost",
	username: "phone",
	password: "test-password",
	iceServers: [],
};
const methods = ["debug", "error", "info", "log", "trace", "warn"] as const;
const original = {
	debug: console.debug,
	error: console.error,
	info: console.info,
	log: console.log,
	trace: console.trace,
	warn: console.warn,
};
let messages = 0;
for (const method of methods)
	console[method] = (): void => {
		messages = Math.min(messages + 1, 1024);
	};
try {
	await phone.connect(account, events);
	await phone.disconnect();
	await phone.connect(account, events);
	await phone.disconnect();
	await assert.rejects(
		phone.connect({ ...account, password: "incorrect-password" }, events),
	);
} finally {
	try {
		await phone.disconnect();
	} finally {
		for (const method of methods) console[method] = original[method];
	}
}
assert.equal(
	messages,
	0,
	"Signaling must not write built-in or transport logs.",
);
console.log(
	"browser registration: actual signaling challenge, reconnect, and rejection completed",
);
