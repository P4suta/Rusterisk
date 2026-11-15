import "./sip-license.ts";
import { type BrowserCapture, microphone } from "./capture.ts";
import { AccountFieldError, connectionOptions, destination } from "./config.ts";
import { AccountValidation } from "./field-validation.ts";
import {
	Phone,
	type PhonePort,
	type PhoneProblem,
	type PhoneState,
} from "./phone.ts";
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
const startupHelp = element("startup-help", HTMLParagraphElement);
const connectionError = element("connection-error", HTMLParagraphElement);
const callError = element("call-error", HTMLParagraphElement);
const connectionStatus = element("connection-status", HTMLParagraphElement);
const callStatus = element("call-status", HTMLParagraphElement);
const fields = {
	server: element("server", HTMLInputElement),
	address: element("address", HTMLInputElement),
	username: element("username", HTMLInputElement),
	password: element("password", HTMLInputElement),
	ice: element("ice", HTMLTextAreaElement),
};
const validation = new AccountValidation(fields, (): void => {
	report(undefined);
});

function report(problem: PhoneProblem | undefined): void {
	for (const [area, output] of [
		["connection", connectionError],
		["call", callError],
	] satisfies readonly (readonly [
		PhoneProblem["area"],
		HTMLParagraphElement,
	])[]) {
		output.textContent = problem?.area === area ? problem.message : "";
		output.hidden = output.textContent.length === 0;
	}
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
	report(state.problem);
}

const signaling = new SipPhone({
	async attach(stream: MediaStream): Promise<void> {
		audio.srcObject = stream;
		try {
			await audio.play();
			if (audio.srcObject === stream) playbackHelp.hidden = true;
		} catch {
			if (audio.srcObject === stream) playbackHelp.hidden = false;
		}
	},
	clear(): void {
		audio.pause();
		audio.srcObject = null;
		playbackHelp.hidden = true;
	},
});

const port: PhonePort<BrowserCapture> = {
	connect: signaling.connect.bind(signaling),
	disconnect: signaling.disconnect.bind(signaling),
	call: signaling.call.bind(signaling),
	answer: signaling.answer.bind(signaling),
	hangup: signaling.hangup.bind(signaling),
};
const phone = new Phone(port, microphone, render);
const pending = new Set<Promise<void>>();
let closing: Promise<void> | undefined;

function run(area: PhoneProblem["area"], operation: () => Promise<void>): void {
	if (pending.size >= 4) {
		report({ area, message: "Wait for the current operation to finish." });
		return;
	}
	const job = operation()
		.catch((): void => {
			report({
				area,
				message: "The operation failed. Disconnect and connect again.",
			});
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
			server: fields.server.value,
			address: fields.address.value,
			username: fields.username.value,
			password: fields.password.value,
			ice: fields.ice.value,
		});
		run("connection", (): Promise<void> => phone.connect(options));
	} catch (error: unknown) {
		report({
			area: "connection",
			message:
				error instanceof AccountFieldError
					? error.message
					: "Reload the page to check the connection fields again.",
		});
		if (error instanceof AccountFieldError) validation.reject(error);
	}
});
callForm.addEventListener("submit", (event: SubmitEvent): void => {
	event.preventDefault();
	if (!callForm.reportValidity()) return;
	try {
		const address = destination(element("destination", HTMLInputElement).value);
		run("call", (): Promise<void> => phone.call(address));
	} catch {
		report({
			area: "call",
			message: "Enter a complete destination SIP address.",
		});
	}
});
disconnect.addEventListener("click", (): void => {
	run("connection", (): Promise<void> => phone.disconnect());
});
answer.addEventListener("click", (): void => {
	run("call", (): Promise<void> => phone.answer());
});
hangup.addEventListener("click", (): void => {
	run("call", (): Promise<void> => phone.hangup());
});
window.addEventListener("pagehide", (): void => {
	if (closing) return;
	closing = phone
		.disconnect()
		.catch((): void => {
			report({
				area: "connection",
				message: "The connection could not close.",
			});
		})
		.finally((): void => {
			closing = undefined;
		});
});
render(phone.state);
startupHelp.hidden = true;
