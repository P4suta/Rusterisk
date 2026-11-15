import type { CaptureState } from "./capture-observer.ts";
import { within } from "./deadline.ts";

export async function captureCheckpoint(
	read: () => Promise<CaptureState>,
	operation: () => Promise<void>,
	milliseconds: number = 2000,
): Promise<void> {
	try {
		await operation();
	} catch (primary: unknown) {
		let description: string;
		try {
			description = JSON.stringify(
				await within("Capture diagnostics", read(), milliseconds),
			);
		} catch (error: unknown) {
			description = JSON.stringify({
				unavailable:
					error instanceof Error ? error.name.slice(0, 64) : "Unknown",
			});
		}
		throw new Error(`Native capture state: ${description}`, { cause: primary });
	}
}
