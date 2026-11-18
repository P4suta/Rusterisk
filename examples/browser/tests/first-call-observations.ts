import { performance } from "node:perf_hooks";
import { within } from "./deadline.ts";

type Milestone =
	| "bobOriginateHttpAccepted"
	| "bothBrowsersAnswered"
	| "aliceObserved660HzTone"
	| "bobObserved440HzTone";

interface Observation {
	readonly format: "rusterisk-first-call-observations";
	readonly version: 1;
	readonly clock: "node-performance-now";
	readonly unit: "milliseconds";
	readonly origin: "before-alice-call-click";
	readonly started: boolean;
	readonly milestones: Readonly<Record<Milestone, number | null>>;
}

export class FirstCallObservations {
	readonly #clock: () => number;
	readonly #milestones: Record<Milestone, number | null> = {
		bobOriginateHttpAccepted: null,
		bothBrowsersAnswered: null,
		aliceObserved660HzTone: null,
		bobObserved440HzTone: null,
	};
	#started: number | null = null;
	#elapsed: number = 0;

	constructor(clock: () => number = (): number => performance.now()) {
		this.#clock = clock;
	}

	start(): void {
		if (this.#started !== null)
			throw new Error("First-call observations already started.");
		const now = this.#clock();
		if (!Number.isFinite(now) || now < 0)
			throw new Error("The first-call observation clock is invalid.");
		this.#started = now;
	}

	mark(milestone: Milestone): void {
		if (this.#started === null)
			throw new Error("First-call observations have not started.");
		if (this.#milestones[milestone] !== null)
			throw new Error("The first-call milestone was already observed.");
		const elapsed = this.#clock() - this.#started;
		if (!Number.isFinite(elapsed) || elapsed < this.#elapsed)
			throw new Error("The first-call observation clock moved backwards.");
		if (elapsed > 60000)
			throw new Error("The first-call observation duration exceeds capacity.");
		this.#milestones[milestone] = elapsed;
		this.#elapsed = elapsed;
	}

	snapshot(): Observation {
		return {
			format: "rusterisk-first-call-observations",
			version: 1,
			clock: "node-performance-now",
			unit: "milliseconds",
			origin: "before-alice-call-click",
			started: this.#started !== null,
			milestones: { ...this.#milestones },
		};
	}

	async observeTones(
		alice: () => Promise<void>,
		bob: () => Promise<void>,
	): Promise<void> {
		const observe = async (
			milestone: Milestone,
			operation: () => Promise<void>,
		): Promise<void> => {
			await operation();
			this.mark(milestone);
		};
		const results = await Promise.allSettled([
			observe("aliceObserved660HzTone", alice),
			observe("bobObserved440HzTone", bob),
		]);
		const failures: unknown[] = [];
		for (const result of results)
			if (result.status === "rejected") failures.push(result.reason);
		if (failures.length === 1) throw failures[0];
		if (failures.length > 1)
			throw new AggregateError(failures, "Both observed tone probes failed.", {
				cause: failures[0],
			});
	}
}

export async function withFirstCallObservations(
	observations: FirstCallObservations,
	operation: () => Promise<void>,
	attach: (body: Buffer) => Promise<void>,
): Promise<void> {
	let outcome: { kind: "complete" } | { kind: "failed"; error: unknown } = {
		kind: "complete",
	};
	try {
		await operation();
	} catch (error: unknown) {
		outcome = { kind: "failed", error };
	}
	let attachment: { kind: "saved" } | { kind: "failed"; error: unknown } = {
		kind: "saved",
	};
	try {
		const body = Buffer.from(JSON.stringify(observations.snapshot()));
		if (body.byteLength > 1024)
			throw new Error("First-call observations exceed attachment capacity.");
		await within("First-call observation attachment", attach(body), 2000);
	} catch (error: unknown) {
		attachment = { kind: "failed", error };
	}
	if (attachment.kind === "failed") {
		if (outcome.kind === "failed")
			throw new AggregateError(
				[outcome.error, attachment.error],
				"The browser call and observation attachment failed.",
				{ cause: outcome.error },
			);
		throw attachment.error;
	}
	if (outcome.kind === "failed") throw outcome.error;
}
