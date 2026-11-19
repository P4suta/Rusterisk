import assert from "node:assert/strict";
import {
	type Browser,
	type BrowserContext,
	expect,
	type Page,
	type Route,
	test,
} from "@playwright/test";
import { receivedTone } from "./audio.ts";
import { browserFailure, launchBrowser } from "./browser.ts";
import { BrowserProcesses, usingBrowsers } from "./browser-lifetime.ts";
import type { BrowserProcess } from "./browser-process.ts";
import { usingCallChannels } from "./call-channels.ts";
import { captureCheckpoint } from "./capture-checkpoint.ts";
import {
	type CaptureState,
	captureAttempts,
	captureState,
	captureStopped,
	observeCapture,
} from "./capture-observer.ts";
import { verifyConnectionRecovery } from "./connection-recovery.ts";
import {
	FirstCallObservations,
	withFirstCallObservations,
} from "./first-call-observations.ts";
import { incomingCall } from "./incoming-call.ts";

function target(name: string): string {
	const value = process.env[name];
	if (!value || value.length > 4096)
		throw new Error(`The owned browser fixture must provide ${name}.`);
	return value;
}

function phase<T>(
	name: string,
	operation: () => Promise<T>,
	milliseconds: number = 10000,
): Promise<T> {
	return test.step(name, operation, { timeout: milliseconds });
}

async function command(
	path: string,
	method: "GET" | "POST" | "DELETE" = "POST",
): Promise<Response> {
	const response = await fetch(new URL(path, target("RUSTERISK_BROWSER_API")), {
		method,
		headers: {
			Authorization: `Basic ${Buffer.from("app:test-password").toString("base64")}`,
		},
		signal: AbortSignal.timeout(4000),
	});
	assert.ok(
		response.ok || (method === "DELETE" && response.status === 404),
		`The browser fixture control request failed: ${String(response.status)}.`,
	);
	return response;
}

async function channelIds(): Promise<readonly string[]> {
	const response = await command("channels", "GET");
	const value: unknown = await response.json();
	if (!Array.isArray(value) || value.length > 8)
		throw new Error("The browser fixture received invalid channel state.");
	const channels: readonly unknown[] = value;
	const ids = channels.map((channel: unknown): string => {
		if (
			typeof channel !== "object" ||
			channel === null ||
			!("id" in channel) ||
			typeof channel.id !== "string" ||
			channel.id.length === 0 ||
			channel.id.length > 128
		)
			throw new Error("The browser fixture received an invalid channel ID.");
		return channel.id;
	});
	assert.equal(new Set(ids).size, ids.length, "Channel IDs must be unique.");
	return ids;
}

async function outgoing(): Promise<string | undefined> {
	return (await channelIds()).find((id: string) => id !== "browser-incoming");
}

function usingBrowserCalls(
	processes: BrowserProcesses,
	operation: () => Promise<void>,
): Promise<void> {
	const requireEmpty = async (): Promise<void> => {
		await expect.poll(channelIds).toEqual([]);
	};
	return usingCallChannels(
		{
			list: channelIds,
			hangup: async (id: string): Promise<void> => {
				await command(`channels/${encodeURIComponent(id)}`, "DELETE");
			},
			requireEmpty,
		},
		(): Promise<void> =>
			usingBrowsers(processes, async (): Promise<void> => {
				await operation();
				await requireEmpty();
			}),
	);
}

async function connect(page: Page, name: string): Promise<void> {
	await test.step(
		`${name}: register the browser account`,
		async (): Promise<void> => {
			await page.goto(target("RUSTERISK_BROWSER_UI"));
			await page
				.getByLabel("WebSocket server URL")
				.fill(target("RUSTERISK_BROWSER_SIGNALING"));
			await page
				.getByLabel("Account SIP address")
				.fill(`sip:${name}@localhost`);
			await page.getByLabel("Authentication username").fill(name);
			await page.getByLabel("Password", { exact: true }).fill("test-password");
			await page.getByRole("button", { name: "Connect", exact: true }).click();
			await expect(page.locator("#connection-status")).toHaveText(
				"Connected and registered",
			);
			await expect(page.locator("#connection-error")).toBeHidden();
		},
		{ timeout: 12000 },
	);
}

async function context(browser: Browser): Promise<BrowserContext> {
	const owned = await browser.newContext();
	owned.setDefaultTimeout(5000);
	owned.setDefaultNavigationTimeout(5000);
	await observeCapture(owned);
	const ui = new URL(target("RUSTERISK_BROWSER_UI"));
	await owned.route("**/*", async (route: Route): Promise<void> => {
		if (new URL(route.request().url()).origin === ui.origin)
			await route.continue();
		else await route.abort("blockedbyclient");
	});
	return owned;
}

async function received(
	page: Page,
	wanted: number,
	other: number,
): Promise<void> {
	await phase(
		`Receive the ${String(wanted)} Hz tone through native media`,
		async (): Promise<void> => {
			await expect
				.poll(
					(): Promise<boolean> =>
						page
							.locator("#remote-audio")
							.evaluate(
								(element: HTMLElement | SVGElement): boolean =>
									element instanceof HTMLAudioElement &&
									element.srcObject instanceof MediaStream,
							),
				)
				.toBe(true);
			await expect
				.poll(
					async (): Promise<boolean> => {
						const value = await page.evaluate(
							async (): Promise<{ rate: number; samples: number[] }> => {
								const audio = document.getElementById("remote-audio");
								if (
									!(audio instanceof HTMLAudioElement) ||
									!(audio.srcObject instanceof MediaStream)
								)
									return { rate: 0, samples: [] };
								const audioContext = new AudioContext({ sampleRate: 48000 });
								const source = audioContext.createMediaStreamSource(
									audio.srcObject,
								);
								const analyser = audioContext.createAnalyser();
								analyser.fftSize = 8192;
								source.connect(analyser);
								try {
									await audioContext.resume();
									await new Promise<void>((resolve: () => void): void => {
										setTimeout(resolve, 250);
									});
									const samples = new Float32Array(analyser.fftSize);
									analyser.getFloatTimeDomainData(samples);
									return {
										rate: audioContext.sampleRate,
										samples: Array.from(samples),
									};
								} finally {
									source.disconnect();
									analyser.disconnect();
									await audioContext.close();
								}
							},
						);
						return receivedTone(value.samples, value.rate, wanted, other);
					},
					{
						message:
							"The received stream must contain the other browser's known tone.",
					},
				)
				.toBe(true);
		},
	);
}

test("actual outgoing and incoming browser calls exchange known audio and save a recording", async (): Promise<void> => {
	const processes = new BrowserProcesses();
	const observations: BrowserProcess[] = [];
	const callObservations = new FirstCallObservations();
	let messages = 0;
	const operation = usingBrowserCalls(processes, async (): Promise<void> => {
		const alice = await launchBrowser(
			"Alice",
			target("RUSTERISK_BROWSER_ALICE_AUDIO"),
			processes,
			observations,
		);
		const bob = await launchBrowser(
			"Bob",
			target("RUSTERISK_BROWSER_BOB_AUDIO"),
			processes,
			observations,
		);
		const left = await test.step(
			"Alice: create the native capture context",
			(): Promise<BrowserContext> => context(alice),
			{ timeout: 8000 },
		);
		const right = await test.step(
			"Bob: create the native capture context",
			(): Promise<BrowserContext> => context(bob),
			{ timeout: 8000 },
		);
		await phase(
			"Set the initial microphone permission decisions",
			async (): Promise<void> => {
				await left.grantPermissions(["microphone"]);
				await right.grantPermissions([]);
			},
		);
		const first = await phase(
			"Alice: create the browser page",
			(): Promise<Page> => left.newPage(),
		);
		const second = await phase(
			"Bob: create the browser page",
			(): Promise<Page> => right.newPage(),
		);
		for (const page of [first, second]) {
			page.on("console", (): void => {
				messages = Math.min(messages + 1, 1024);
			});
			page.on("pageerror", (): void => {
				messages = Math.min(messages + 1, 1024);
			});
		}
		await connect(first, "alice");
		await connect(second, "bob");
		const id = await phase(
			"Alice: acquire native microphone audio and send the outgoing call",
			async (): Promise<string> => {
				assert.equal(await captureAttempts(first), 0);
				assert.equal(await captureAttempts(second), 0);
				await first
					.getByLabel("Destination SIP address")
					.fill("sip:100@localhost");
				callObservations.start();
				await first.getByRole("button", { name: "Call", exact: true }).click();
				let id: string | undefined;
				await captureCheckpoint(
					(): Promise<CaptureState> => captureState(first),
					async (): Promise<void> => {
						await expect
							.poll(async (): Promise<boolean> => {
								id = await outgoing();
								return id !== undefined;
							})
							.toBe(true);
					},
				);
				assert.ok(id);
				return id;
			},
			12000,
		);
		await phase(
			"Alice: answer the outgoing SIP call",
			async (): Promise<void> => {
				await command(`channels/${encodeURIComponent(id)}/answer`);
				await captureCheckpoint(
					(): Promise<CaptureState> => captureState(first),
					async (): Promise<void> => {
						await expect(first.locator("#call-status")).toHaveText(
							"Call answered",
						);
					},
				);
			},
		);
		await phase(
			"Bob: receive the incoming SIP call",
			async (): Promise<void> => {
				await command(
					incomingCall(
						process.env.RUSTERISK_BROWSER_SERVER,
						test.info().timeout,
					),
				);
				callObservations.mark("bobOriginateHttpAccepted");
				await expect(
					second.getByRole("button", { name: "Answer", exact: true }),
				).toBeEnabled();
				assert.equal(await captureAttempts(second), 0);
			},
		);
		await phase(
			"Bob: deny microphone capture without losing the incoming call",
			async (): Promise<void> => {
				await second
					.getByRole("button", { name: "Answer", exact: true })
					.click();
				await expect(second.locator("#call-error")).toContainText(
					"Microphone permission was denied.",
				);
				await expect(
					second.getByRole("button", { name: "Answer", exact: true }),
				).toBeEnabled();
			},
		);
		await phase(
			"Bob: grant microphone capture and answer the incoming call",
			async (): Promise<void> => {
				await right.grantPermissions(["microphone"]);
				await second
					.getByRole("button", { name: "Answer", exact: true })
					.click();
				await captureCheckpoint(
					(): Promise<CaptureState> => captureState(second),
					async (): Promise<void> => {
						await expect(second.locator("#call-status")).toHaveText(
							"Call answered",
						);
					},
				);
				callObservations.mark("bothBrowsersAnswered");
			},
		);
		await phase(
			"Bridge both answered calls and start the native recording",
			async (): Promise<void> => {
				await command("bridges?bridgeId=browser-mix&type=mixing");
				await command(
					`bridges/browser-mix/addChannel?channel=${encodeURIComponent(id)},browser-incoming`,
				);
				await command(
					"applications/calls/subscription?eventSource=bridge%3Abrowser-mix",
				);
				await command(
					"bridges/browser-mix/record?name=browser-known&format=wav&maxDurationSeconds=10",
				);
			},
		);
		await callObservations.observeTones(
			(): Promise<void> => received(first, 660, 440),
			(): Promise<void> => received(second, 440, 660),
		);
		await phase(
			"Stop the recording after both native streams arrive",
			async (): Promise<void> => {
				assert.equal(await captureAttempts(first), 1);
				assert.equal(await captureAttempts(second), 2);
				await command("recordings/live/browser-known/stop");
			},
		);
		for (const page of [first, second]) {
			await phase(
				"End the call, release microphone tracks, and disconnect",
				async (): Promise<void> => {
					await page
						.getByRole("button", { name: "End call", exact: true })
						.click();
					await expect(page.locator("#call-status")).toHaveText(
						"No active call",
					);
					await expect
						.poll((): Promise<boolean> => captureStopped(page))
						.toBe(true);
					await page
						.getByRole("button", { name: "Disconnect", exact: true })
						.click();
					await expect(page.locator("#connection-status")).toHaveText(
						"Disconnected",
					);
				},
			);
		}
		assert.equal(
			messages,
			0,
			"Browser calls must not log signaling, credentials, or media.",
		);
	}).catch(
		(primary: unknown): Promise<never> => browserFailure(primary, observations),
	);
	await withFirstCallObservations(
		callObservations,
		(): Promise<void> => operation,
		(body: Buffer): Promise<void> =>
			test.info().attach("first-call-observations", {
				body,
				contentType: "application/json",
			}),
	);
});

test("cancelling or disconnecting during native capture releases late tracks and preserves reconnect", async (): Promise<void> => {
	const processes = new BrowserProcesses();
	const observations: BrowserProcess[] = [];
	await usingBrowserCalls(processes, async (): Promise<void> => {
		const browser = await launchBrowser(
			"Alice",
			target("RUSTERISK_BROWSER_ALICE_AUDIO"),
			processes,
			observations,
		);
		const owned = await test.step(
			"Alice: create the native capture context",
			(): Promise<BrowserContext> => context(browser),
			{ timeout: 8000 },
		);
		await phase(
			"Alice: grant microphone permission",
			(): Promise<void> => owned.grantPermissions(["microphone"]),
		);
		const page = await phase(
			"Alice: create the browser page",
			(): Promise<Page> => owned.newPage(),
		);
		await connect(page, "alice");
		for (const action of ["End call", "Disconnect"]) {
			await phase(
				`${action} while native microphone capture is pending`,
				async (): Promise<void> => {
					await page
						.getByLabel("Destination SIP address")
						.fill("sip:100@localhost");
					await page.evaluate((): void => {
						window.rusteriskCapture.hold = true;
					});
					await page.getByRole("button", { name: "Call", exact: true }).click();
					await captureCheckpoint(
						(): Promise<CaptureState> => captureState(page),
						async (): Promise<void> => {
							await expect
								.poll((): Promise<CaptureState> => captureState(page))
								.toMatchObject({ waiting: true });
						},
					);
					await page.getByRole("button", { name: action, exact: true }).click();
					await expect(page.locator("#call-status")).toHaveText(
						"No active call",
					);
					await page.evaluate((): void => {
						const capture = window.rusteriskCapture;
						capture.hold = false;
						const release = capture.release;
						capture.release = undefined;
						release?.();
					});
					await expect
						.poll((): Promise<boolean> => captureStopped(page))
						.toBe(true);
					assert.deepEqual(await channelIds(), []);
					if (action === "Disconnect") {
						await expect(page.locator("#connection-status")).toHaveText(
							"Disconnected",
						);
						await page
							.getByRole("button", { name: "Connect", exact: true })
							.click();
						await expect(page.locator("#connection-status")).toHaveText(
							"Connected and registered",
						);
					}
				},
				15000,
			);
		}
		await phase(
			"Cancel an outgoing SIP call and release the native microphone",
			async (): Promise<void> => {
				await page.getByRole("button", { name: "Call", exact: true }).click();
				await expect.poll(outgoing).toBeDefined();
				await page
					.getByRole("button", { name: "End call", exact: true })
					.click();
				await expect(page.locator("#call-status")).toHaveText("No active call");
				await expect
					.poll((): Promise<boolean> => captureStopped(page))
					.toBe(true);
				await expect(page.locator("#call-error")).toBeHidden();
				await expect.poll(outgoing).toBeUndefined();
				await page
					.getByRole("button", { name: "Disconnect", exact: true })
					.click();
				await expect(page.locator("#connection-status")).toHaveText(
					"Disconnected",
				);
			},
			15000,
		);
		await phase(
			"Recover field validation and authentication with the keyboard",
			(): Promise<void> => verifyConnectionRecovery(page),
			20000,
		);
	}).catch(
		(primary: unknown): Promise<never> => browserFailure(primary, observations),
	);
});
