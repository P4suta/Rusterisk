import type { PhoneState } from "./phone.ts";

export interface Presentation {
	readonly connection: string;
	readonly call: string;
	readonly account: boolean;
	readonly destination: boolean;
	readonly connect: boolean;
	readonly disconnect: boolean;
	readonly dial: boolean;
	readonly answer: boolean;
	readonly hangup: boolean;
}

export function presentation(state: PhoneState): Presentation {
	const connection: Readonly<Record<PhoneState["connection"], string>> = {
		disconnected: "Disconnected",
		connecting: "Connecting and registering",
		registered: "Connected and registered",
		disconnecting: "Disconnecting",
	};
	const call: Readonly<Record<PhoneState["call"], string>> = {
		idle: "No active call",
		"call-microphone": "Waiting for microphone permission",
		"answer-microphone": "Waiting for microphone permission to answer",
		calling: "Calling",
		incoming: state.caller
			? `Incoming call from ${state.caller}`
			: "Incoming call",
		answering: "Answering",
		active: "Call answered",
		ending: "Ending call",
	};
	const ready = state.connection === "registered";
	return {
		connection: connection[state.connection],
		call: call[state.call],
		account: state.connection === "disconnected",
		destination: ready && state.call === "idle",
		connect: state.connection === "disconnected",
		disconnect: state.connection === "connecting" || ready,
		dial: ready && state.call === "idle",
		answer: ready && state.call === "incoming",
		hangup: ready && state.call !== "idle" && state.call !== "ending",
	};
}
