export function incomingCall(server: string | undefined): string {
	const target = server ?? "rusterisk";
	if (target !== "rusterisk" && target !== "asterisk") {
		throw new Error("Unknown browser verification server.");
	}
	const query = new URLSearchParams({
		endpoint: target === "asterisk" ? "PJSIP/bob" : "PJSIP/bob-endpoint",
		app: "calls",
		channelId: "browser-incoming",
		callerId: "100",
		timeout: "8",
	});
	return `channels?${query.toString()}`;
}
