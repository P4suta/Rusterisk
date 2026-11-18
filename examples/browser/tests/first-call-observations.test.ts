import assert from "node:assert/strict";
import test from "node:test";
import {
	FirstCallObservations,
	withFirstCallObservations,
} from "./first-call-observations.ts";

await test("first-call observations retain bounded monotonic milestones for observed tones", (): void => {
	let now = 1000;
	const observations = new FirstCallObservations((): number => now);
	assert.equal(observations.snapshot().started, false);
	observations.start();
	now = 1100;
	observations.mark("bobOriginateHttpAccepted");
	now = 1400;
	observations.mark("bothBrowsersAnswered");
	now = 1600;
	observations.mark("bobObserved440HzTone");
	now = 1750;
	observations.mark("aliceObserved660HzTone");
	assert.deepEqual(observations.snapshot(), {
		format: "rusterisk-first-call-observations",
		version: 1,
		clock: "node-performance-now",
		unit: "milliseconds",
		origin: "before-alice-call-click",
		started: true,
		milestones: {
			bobOriginateHttpAccepted: 100,
			bothBrowsersAnswered: 400,
			aliceObserved660HzTone: 750,
			bobObserved440HzTone: 600,
		},
	});
});

await test("incomplete observations keep missing milestones distinct from zero", (): void => {
	const observations = new FirstCallObservations((): number => 20);
	observations.start();
	observations.mark("bobOriginateHttpAccepted");
	assert.deepEqual(observations.snapshot().milestones, {
		bobOriginateHttpAccepted: 0,
		bothBrowsersAnswered: null,
		aliceObserved660HzTone: null,
		bobObserved440HzTone: null,
	});
	assert.throws((): void => {
		observations.start();
	}, /already started/u);
	assert.throws((): void => {
		observations.mark("bobOriginateHttpAccepted");
	}, /already observed/u);
});

await test("observations reject missing origins, reversed clocks, invalid numbers, and excessive durations", (): void => {
	for (const initial of [Number.NaN, Number.POSITIVE_INFINITY, -1]) {
		const observations = new FirstCallObservations((): number => initial);
		assert.throws((): void => {
			observations.start();
		}, /clock/u);
	}
	for (const invalid of [99, Number.NaN, Number.POSITIVE_INFINITY, 60101]) {
		let now = 100;
		const observations = new FirstCallObservations((): number => now);
		assert.throws((): void => {
			observations.mark("bobOriginateHttpAccepted");
		}, /not started/u);
		observations.start();
		now = invalid;
		assert.throws((): void => {
			observations.mark("bobOriginateHttpAccepted");
		}, /clock|duration/u);
		assert.equal(
			observations.snapshot().milestones.bobOriginateHttpAccepted,
			null,
		);
	}
});

await test("tone probes start together and both settle before their failures escape", async (): Promise<void> => {
	let now = 100;
	const observations = new FirstCallObservations((): number => now);
	observations.start();
	observations.mark("bobOriginateHttpAccepted");
	observations.mark("bothBrowsersAnswered");
	for (const bobFails of [false, true]) {
		const aliceFailure = new Error("Alice did not observe the expected tone.");
		const bobFailure = new Error("Bob did not observe the expected tone.");
		let finishBob: (() => void) | undefined;
		const pendingBob = new Promise<void>((resolve: () => void): void => {
			finishBob = resolve;
		});
		const started: string[] = [];
		let settled = false;
		const probing = observations
			.observeTones(
				(): Promise<void> => {
					started.push("alice");
					throw aliceFailure;
				},
				async (): Promise<void> => {
					started.push("bob");
					await pendingBob;
					now += 10;
					if (bobFails) throw bobFailure;
				},
			)
			.finally((): void => {
				settled = true;
			});
		const checked = assert.rejects(probing, (error: unknown): boolean => {
			if (bobFails) {
				assert.ok(error instanceof AggregateError);
				assert.deepEqual(error.errors, [aliceFailure, bobFailure]);
				assert.equal(error.cause, aliceFailure);
			} else assert.equal(error, aliceFailure);
			return true;
		});
		await Promise.resolve();
		assert.deepEqual(started, ["alice", "bob"]);
		assert.equal(settled, false);
		assert.ok(finishBob);
		finishBob();
		await checked;
		assert.equal(settled, true);
	}
	assert.equal(observations.snapshot().milestones.aliceObserved660HzTone, null);
	assert.equal(observations.snapshot().milestones.bobObserved440HzTone, 10);
});

await test("successful concurrent tone probes retain each observation time", async (): Promise<void> => {
	let now = 0;
	const observations = new FirstCallObservations((): number => now);
	observations.start();
	observations.mark("bobOriginateHttpAccepted");
	observations.mark("bothBrowsersAnswered");
	await observations.observeTones(
		(): Promise<void> => {
			now = 40;
			return Promise.resolve();
		},
		async (): Promise<void> => {
			await Promise.resolve();
			now = 55;
		},
	);
	assert.equal(observations.snapshot().milestones.aliceObserved660HzTone, 40);
	assert.equal(observations.snapshot().milestones.bobObserved440HzTone, 55);
});

await test("structured attachments retain partial observations and preserve operation and attachment failures", async (): Promise<void> => {
	for (const operationFails of [false, true]) {
		for (const attachmentFails of [false, true]) {
			const observations = new FirstCallObservations((): number => 100);
			const primary = new Error("The call failed before an observed tone.");
			const attachment = new Error("The observation attachment failed.");
			let body: Buffer | undefined;
			const result = withFirstCallObservations(
				observations,
				(): Promise<void> => {
					observations.start();
					observations.mark("bobOriginateHttpAccepted");
					return operationFails ? Promise.reject(primary) : Promise.resolve();
				},
				(bytes: Buffer): Promise<void> => {
					body = bytes;
					return attachmentFails
						? Promise.reject(attachment)
						: Promise.resolve();
				},
			);
			if (operationFails || attachmentFails)
				await assert.rejects(result, (error: unknown): boolean => {
					if (operationFails && attachmentFails) {
						assert.ok(error instanceof AggregateError);
						assert.deepEqual(error.errors, [primary, attachment]);
						assert.equal(error.cause, primary);
					} else assert.equal(error, operationFails ? primary : attachment);
					return true;
				});
			else await result;
			assert.ok(body);
			assert.ok(body.byteLength <= 1024);
			const value: unknown = JSON.parse(body.toString("utf8"));
			assert.deepEqual(value, observations.snapshot());
			assert.doesNotMatch(body.toString("utf8"), /latency|test-password/u);
		}
	}
});
