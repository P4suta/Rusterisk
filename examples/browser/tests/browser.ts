import assert from "node:assert/strict";
import {
	type Browser,
	type BrowserServer,
	chromium,
	type LaunchOptions,
	test,
} from "@playwright/test";
import type { BrowserProcesses } from "./browser-lifetime.ts";
import { BrowserProcess, type BrowserProcessState } from "./browser-process.ts";
import { within } from "./deadline.ts";

export const browserChannel: string = "chromium";

export function nativeBrowserOptions(audio: string): LaunchOptions {
	return {
		channel: browserChannel,
		headless: true,
		args: [
			"--allow-loopback-in-peer-connection",
			"--use-fake-device-for-media-stream",
			`--use-file-for-fake-audio-capture=${audio}`,
		],
	};
}

export function checkBrowser(browser: Pick<Browser, "version">): void {
	assert.equal(browser.version(), "159.0.8154.6");
}

export function automationFailure(error: unknown): Error {
	const name = error instanceof Error ? error.name.slice(0, 64) : "Unknown";
	return new Error(`Owned Chromium automation connection failed: ${name}.`);
}

export async function launchBrowser(
	name: "Alice" | "Bob",
	audio: string,
	processes: BrowserProcesses,
	observations: BrowserProcess[],
): Promise<Browser> {
	const server = await test.step(
		`${name}: launch owned Chromium`,
		async (): Promise<BrowserProcess> => {
			return await processes.acquire(
				chromium
					.launchServer({
						...nativeBrowserOptions(audio),
						host: "127.0.0.1",
						port: 0,
						timeout: 10000,
					})
					.then((server: BrowserServer): BrowserProcess => {
						const observed = new BrowserProcess(name, server);
						observations.push(observed);
						return observed;
					}),
			);
		},
		{ timeout: 12000 },
	);
	const browser = await test.step(
		`${name}: connect owned Chromium automation`,
		async (): Promise<Browser> => {
			const connected = await chromium
				.connect(server.wsEndpoint(), { timeout: 5000 })
				.catch(automationFailure);
			if (connected instanceof Error) throw connected;
			return connected;
		},
		{ timeout: 7000 },
	);
	checkBrowser(browser);
	return browser;
}

export async function browserFailure(
	primary: unknown,
	observations: readonly BrowserProcess[],
): Promise<never> {
	const body = Buffer.from(
		JSON.stringify(
			observations.map(
				(observed: BrowserProcess): BrowserProcessState => observed.state(),
			),
		),
	);
	if (body.byteLength > 4096)
		throw new Error("Owned browser process diagnostics exceed capacity.", {
			cause: primary,
		});
	let attachment: { kind: "saved" } | { kind: "failed"; error: unknown } = {
		kind: "saved",
	};
	try {
		await within(
			"Browser process diagnostics",
			test
				.info()
				.attach("owned-browser-processes", { body, contentType: "text/plain" }),
			2000,
		);
	} catch (error: unknown) {
		attachment = { kind: "failed", error };
	}
	if (attachment.kind === "failed")
		throw new AggregateError(
			[primary, attachment.error],
			"The native browser operation and process diagnostics failed.",
			{ cause: primary },
		);
	throw primary;
}
