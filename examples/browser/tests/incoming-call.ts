export function incomingCall(
	server: string | undefined,
	caseTimeoutMilliseconds: number,
): string {
	const target = server ?? "rusterisk";
	if (target !== "rusterisk" && target !== "asterisk") {
		throw new Error("Unknown browser verification server.");
	}
	const seconds = Math.ceil(caseTimeoutMilliseconds / 1000);
	if (
		!Number.isSafeInteger(caseTimeoutMilliseconds) ||
		seconds < 1 ||
		seconds > 3600
	)
		throw new Error("Invalid incoming call timeout for the browser case.");
	const query = new URLSearchParams({
		endpoint: target === "asterisk" ? "PJSIP/bob" : "PJSIP/bob-endpoint",
		app: "calls",
		channelId: "browser-incoming",
		callerId: "100",
		timeout: String(seconds),
	});
	return `channels?${query.toString()}`;
}
