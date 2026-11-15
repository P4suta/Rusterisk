import type { BrowserServer } from "@playwright/test";
import { within } from "./deadline.ts";

type OwnedProcess = Pick<BrowserServer, "kill">;

export class BrowserProcesses {
	readonly #processes: OwnedProcess[] = [];
	readonly #milliseconds: number;
	#accepting: boolean = true;

	constructor(milliseconds: number = 3000) {
		this.#milliseconds = milliseconds;
	}

	async acquire<T extends OwnedProcess>(launch: Promise<T>): Promise<T> {
		const process = await launch;
		if (!this.#accepting) {
			await within("Late browser kill", process.kill(), this.#milliseconds);
			throw new Error("The browser finished launching after its scope closed.");
		}
		this.#processes.push(process);
		return process;
	}

	async close(): Promise<unknown[]> {
		this.#accepting = false;
		const results = await Promise.allSettled(
			this.#processes
				.splice(0)
				.map(async (process: OwnedProcess): Promise<void> => {
					await within(
						"Owned browser kill",
						process.kill(),
						this.#milliseconds,
					);
				}),
		);
		const failures: unknown[] = [];
		for (const result of results)
			if (result.status === "rejected") failures.push(result.reason);
		return failures;
	}
}

export async function usingBrowsers(
	processes: BrowserProcesses,
	operation: () => Promise<void>,
): Promise<void> {
	let outcome: { kind: "complete" } | { kind: "failed"; error: unknown } = {
		kind: "complete",
	};
	try {
		await operation();
	} catch (error: unknown) {
		outcome = { kind: "failed", error };
	}
	const failures = await processes.close();
	if (failures.length > 0) {
		if (outcome.kind === "failed")
			throw new AggregateError(
				[outcome.error, ...failures],
				"The native browser operation and owned process cleanup failed.",
				{ cause: outcome.error },
			);
		throw new AggregateError(failures, "Owned browser process cleanup failed.");
	}
	if (outcome.kind === "failed") throw outcome.error;
}
