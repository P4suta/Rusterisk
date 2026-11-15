import type { BrowserContext, Page } from "@playwright/test";

interface CaptureObservation {
	attempts: number;
	streams: MediaStream[];
	hold: boolean;
	release: (() => void) | undefined;
	failure: string | undefined;
}

export interface CaptureState {
	readonly attempts: number;
	readonly streams: number;
	readonly waiting: boolean;
	readonly failure: string;
	readonly secure: boolean;
	readonly call: string;
	readonly problem: string;
}

declare global {
	interface Window {
		rusteriskCapture: CaptureObservation;
	}
}

export async function observeCapture(context: BrowserContext): Promise<void> {
	await context.addInitScript((): void => {
		const observation: CaptureObservation = {
			attempts: 0,
			streams: [],
			hold: false,
			release: undefined,
			failure: undefined,
		};
		window.rusteriskCapture = observation;
		const acquire = navigator.mediaDevices.getUserMedia.bind(
			navigator.mediaDevices,
		);
		navigator.mediaDevices.getUserMedia = async (
			constraints?: MediaStreamConstraints,
		): Promise<MediaStream> => {
			if (
				observation.attempts >= 8 ||
				!constraints?.audio ||
				constraints.video !== false
			)
				throw new Error(
					"The browser fixture requires bounded explicit audio-only capture.",
				);
			observation.attempts += 1;
			observation.failure = undefined;
			const audio =
				typeof constraints.audio === "object" ? constraints.audio : {};
			const stream = await acquire({
				audio: {
					...audio,
					echoCancellation: false,
					noiseSuppression: false,
					autoGainControl: false,
				},
				video: false,
			}).catch((error: unknown): never => {
				observation.failure =
					error instanceof DOMException ? error.name.slice(0, 64) : "Error";
				throw error;
			});
			observation.streams.push(stream);
			if (observation.hold)
				await new Promise<void>((resolve: () => void): void => {
					observation.release = resolve;
				});
			return stream;
		};
	});
}

export function captureState(page: Page): Promise<CaptureState> {
	return page.evaluate(
		(): CaptureState => ({
			attempts: window.rusteriskCapture.attempts,
			streams: window.rusteriskCapture.streams.length,
			waiting: window.rusteriskCapture.release !== undefined,
			failure: window.rusteriskCapture.failure ?? "",
			secure: globalThis.isSecureContext,
			call:
				document.getElementById("call-status")?.textContent.slice(0, 256) ?? "",
			problem:
				document.getElementById("call-error")?.textContent.slice(0, 256) ?? "",
		}),
	);
}

export function captureAttempts(page: Page): Promise<number> {
	return page.evaluate((): number => window.rusteriskCapture.attempts);
}

export function captureStopped(page: Page): Promise<boolean> {
	return page.evaluate((): boolean =>
		window.rusteriskCapture.streams.every((stream: MediaStream): boolean =>
			stream
				.getTracks()
				.every(
					(track: MediaStreamTrack): boolean => track.readyState === "ended",
				),
		),
	);
}
