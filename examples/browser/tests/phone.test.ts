import assert from "node:assert/strict";
import test from "node:test";
import {
	type CallEvents,
	type Capture,
	type ConnectionOptions,
	Phone,
	type PhonePort,
	type SessionEvents,
} from "../src/phone.ts";

class Port implements PhonePort<Capture> {
	public events: CallEvents | undefined;
	public calls: string[] = [];
	public capture: Capture | undefined;
	public session: SessionEvents | undefined;
	public connect(
		_options: ConnectionOptions,
		events: CallEvents,
	): Promise<void> {
		this.events = events;
		this.calls.push("connect");
		return Promise.resolve();
	}
	public disconnect(): Promise<void> {
		this.calls.push("disconnect");
		return Promise.resolve();
	}
	public call(
		destination: string,
		capture: Capture,
		events: SessionEvents,
	): Promise<void> {
		this.capture = capture;
		this.session = events;
		this.calls.push(destination);
		return Promise.resolve();
	}
	public answer(capture: Capture): Promise<void> {
		this.capture = capture;
		this.calls.push("answer");
		return Promise.resolve();
	}
	public hangup(): Promise<void> {
		this.calls.push("hangup");
		return Promise.resolve();
	}
}

const options: ConnectionOptions = {
	server: "ws://127.0.0.1:8088/ws",
	address: "sip:alice@localhost",
	username: "alice",
	password: "test-password",
	iceServers: [],
};

await test("connecting and incoming calls never acquire the microphone before an explicit action", async (): Promise<void> => {
	const port = new Port();
	let captures = 0;
	let stops = 0;
	const phone = new Phone(
		port,
		(): Promise<Capture> => {
			captures += 1;
			return Promise.resolve({
				stop(): void {
					stops += 1;
				},
			});
		},
		(): void => {},
	);
	await phone.connect(options);
	assert.equal(captures, 0);
	const call = port.events?.incoming("Caller");
	assert.equal(phone.state.call, "incoming");
	assert.equal(captures, 0);
	await phone.answer();
	assert.equal(captures, 1);
	call?.answered();
	assert.equal(phone.state.call, "active");
	call?.ended();
	assert.equal(stops, 1);
	assert.equal(phone.state.call, "idle");
	await phone.disconnect();
});

await test("an incoming call without a caller label is rejected when microphone acquisition is cancelled", async (): Promise<void> => {
	const port = new Port();
	let finish: ((capture: Capture) => void) | undefined;
	let stops = 0;
	const phone = new Phone(
		port,
		(): Promise<Capture> =>
			new Promise((resolve: (capture: Capture) => void): void => {
				finish = resolve;
			}),
		(): void => {},
	);
	await phone.connect(options);
	const session = port.events?.incoming("");
	const answer = phone.answer();
	await phone.hangup();
	assert.deepEqual(port.calls, ["connect", "hangup"]);
	session?.ended();
	assert.ok(finish);
	finish({
		stop(): void {
			stops += 1;
		},
	});
	await answer;
	assert.equal(stops, 1);
	assert.equal(phone.state.call, "idle");
	await phone.disconnect();
});

await test("late callbacks from an ended call cannot release a replacement capture", async (): Promise<void> => {
	const port = new Port();
	let stops = 0;
	const phone = new Phone(
		port,
		(): Promise<Capture> =>
			Promise.resolve({
				stop(): void {
					stops += 1;
				},
			}),
		(): void => {},
	);
	await phone.connect(options);
	await phone.call("sip:100@localhost");
	const original = port.session;
	original?.ended();
	await phone.call("sip:101@localhost");
	assert.equal(stops, 1);
	original?.answered();
	original?.ended();
	assert.equal(stops, 1);
	assert.equal(phone.state.call, "calling");
	await phone.disconnect();
	assert.equal(stops, 2);
});

await test("microphone denial leaves recovery available without sending an outgoing call", async (): Promise<void> => {
	const port = new Port();
	let denied = true;
	let stops = 0;
	const phone = new Phone(
		port,
		(): Promise<Capture> => {
			if (denied)
				return Promise.reject(new DOMException("denied", "NotAllowedError"));
			return Promise.resolve({
				stop(): void {
					stops += 1;
				},
			});
		},
		(): void => {},
	);
	await phone.connect(options);
	await phone.call("sip:100@localhost");
	assert.equal(phone.state.call, "idle");
	assert.match(phone.state.error, /microphone permission/i);
	assert.deepEqual(port.calls, ["connect"]);
	denied = false;
	await phone.call("sip:100@localhost");
	assert.equal(phone.state.call, "calling");
	await phone.hangup();
	assert.equal(stops, 1);
	await phone.disconnect();
});

await test("disconnect during microphone acquisition stops the late capture and never starts a call", async (): Promise<void> => {
	const port = new Port();
	let finish: ((capture: Capture) => void) | undefined;
	let stops = 0;
	const phone = new Phone(
		port,
		(): Promise<Capture> =>
			new Promise((resolve: (capture: Capture) => void): void => {
				finish = resolve;
			}),
		(): void => {},
	);
	await phone.connect(options);
	const call = phone.call("sip:100@localhost");
	await phone.disconnect();
	assert.ok(finish);
	finish({
		stop(): void {
			stops += 1;
		},
	});
	await call;
	assert.equal(stops, 1);
	assert.equal(phone.state.connection, "disconnected");
	assert.deepEqual(port.calls, ["connect", "disconnect"]);
});
