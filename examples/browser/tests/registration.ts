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
try {
	await phone.connect(account, events);
	await phone.disconnect();
	await phone.connect(account, events);
	await phone.disconnect();
	await assert.rejects(
		phone.connect({ ...account, password: "incorrect-password" }, events),
	);
} finally {
	await phone.disconnect();
}
console.log(
	"browser registration: actual signaling challenge, reconnect, and rejection completed",
);
