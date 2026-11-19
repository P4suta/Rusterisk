import type { Capture } from "./phone.ts";

export interface MediaCapture extends Capture {
	readonly stream: MediaStream;
}

export class BrowserCapture implements MediaCapture {
	public readonly stream: MediaStream;
	public constructor(stream: MediaStream) {
		this.stream = stream;
	}
	public stop(): void {
		for (const track of this.stream.getTracks()) track.stop();
	}
}

function availableDevices(environment: {
	mediaDevices?: Partial<MediaDevices>;
}): Partial<MediaDevices> | undefined {
	return environment.mediaDevices;
}

export async function microphone(): Promise<BrowserCapture> {
	const devices = availableDevices(navigator);
	if (!globalThis.isSecureContext || !devices?.getUserMedia)
		throw new DOMException(
			"Microphone access requires a secure browser context.",
			"NotSupportedError",
		);
	return new BrowserCapture(
		await devices.getUserMedia({ audio: true, video: false }),
	);
}
