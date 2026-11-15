import { type BrowserCapture, microphone } from "./capture.ts";
import { connectionOptions, destination } from "./config.ts";
import { Phone, type PhonePort, type PhoneState } from "./phone.ts";
import { presentation } from "./presentation.ts";
import { SipPhone } from "./sip.ts";

function element<T extends Element>(id: string, kind: new () => T): T {
	const value = document.getElementById(id);
	if (!(value instanceof kind))
		throw new Error("The browser example markup is incomplete.");
	return value;
}

const connectForm = element("connection-form", HTMLFormElement);
const account = element("connection-fields", HTMLFieldSetElement);
const callForm = element("call-form", HTMLFormElement);
const callFields = element("call-fields", HTMLFieldSetElement);
const connect = element("connect", HTMLButtonElement);
const disconnect = element("disconnect", HTMLButtonElement);
const call = element("call", HTMLButtonElement);
const answer = element("answer", HTMLButtonElement);
const hangup = element("hangup", HTMLButtonElement);
const audio = element("remote-audio", HTMLAudioElement);
const playbackHelp = element("playback-help", HTMLParagraphElement);
const error = element("error", HTMLParagraphElement);
const connectionStatus = element("connection-status", HTMLParagraphElement);
const callStatus = element("call-status", HTMLParagraphElement);

function report(message: string): void {
	error.textContent = message;
	error.hidden = message.length === 0;
}

function render(state: PhoneState): void {
	const view = presentation(state);
	account.disabled = !view.account;
	callFields.disabled = !view.destination;
	connect.disabled = !view.connect;
	disconnect.disabled = !view.disconnect;
	call.disabled = !view.dial;
	answer.disabled = !view.answer;
	hangup.disabled = !view.hangup;
	if (connectionStatus.textContent !== view.connection)
		connectionStatus.textContent = view.connection;
	if (callStatus.textContent !== view.call) callStatus.textContent = view.call;
	report(state.error);
}

const signaling = new SipPhone({
	async attach(stream: MediaStream): Promise<void> {
		audio.srcObject = stream;
		try {
			await audio.play();
			playbackHelp.hidden = true;
		} catch {
			playbackHelp.hidden = false;
		}
	},
	clear(): void {
		audio.pause();
		audio.srcObject = null;
		playbackHelp.hidden = true;
	},
});

function unavailable(): Promise<void> {
	return Promise.reject(new Error("Browser call media is not connected."));
}

const port: PhonePort<BrowserCapture> = {
	connect: signaling.connect.bind(signaling),
	disconnect: signaling.disconnect.bind(signaling),
	call: unavailable,
	answer: unavailable,
	hangup: unavailable,
};
const phone = new Phone(port, microphone, render);
const pending = new Set<Promise<void>>();
let closing: Promise<void> | undefined;

function run(operation: () => Promise<void>): void {
	if (pending.size >= 4) {
		report("Wait for the current operation to finish.");
		return;
	}
	const job = operation()
		.catch((): void => {
			report("The operation failed. Disconnect and connect again.");
		})
		.finally((): void => {
			pending.delete(job);
		});
	pending.add(job);
}

connectForm.addEventListener("submit", (event: SubmitEvent): void => {
	event.preventDefault();
	if (!connectForm.reportValidity()) return;
	try {
		const options = connectionOptions({
			server: element("server", HTMLInputElement).value,
			address: element("address", HTMLInputElement).value,
			username: element("username", HTMLInputElement).value,
			password: element("password", HTMLInputElement).value,
			ice: element("ice", HTMLTextAreaElement).value,
		});
		run((): Promise<void> => phone.connect(options));
	} catch {
		report(
			"Check the connection fields, including the server URL and ICE settings.",
		);
	}
});
callForm.addEventListener("submit", (event: SubmitEvent): void => {
	event.preventDefault();
	if (!callForm.reportValidity()) return;
	try {
		const address = destination(element("destination", HTMLInputElement).value);
		run((): Promise<void> => phone.call(address));
	} catch {
		report("Enter a complete destination SIP address.");
	}
});
disconnect.addEventListener("click", (): void => {
	run((): Promise<void> => phone.disconnect());
});
answer.addEventListener("click", (): void => {
	run((): Promise<void> => phone.answer());
});
hangup.addEventListener("click", (): void => {
	run((): Promise<void> => phone.hangup());
});
window.addEventListener("pagehide", (): void => {
	if (closing) return;
	closing = phone
		.disconnect()
		.catch((): void => {
			report("The connection could not close.");
		})
		.finally((): void => {
			closing = undefined;
		});
});
render(phone.state);
