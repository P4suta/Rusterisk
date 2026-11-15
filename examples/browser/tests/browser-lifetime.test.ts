import assert from "node:assert/strict";
import test from "node:test";
import { BrowserProcesses, usingBrowsers } from "./browser-lifetime.ts";

await test("every owned browser is killed after successful and failed operations", async (): Promise<void> => {
	for (const fail of [false, true]) {
		const killed: number[] = [];
		const primary = new Error("Native capture did not finish.");
		const operation = (): Promise<void> =>
			fail ? Promise.reject(primary) : Promise.resolve();
		const processes = new BrowserProcesses();
		for (const id of [0, 1])
			await processes.acquire(
				Promise.resolve({
					kill: (): Promise<void> => {
						killed.push(id);
						return Promise.resolve();
					},
				}),
			);
		const result = usingBrowsers(processes, operation);
		if (fail)
			await assert.rejects(
				result,
				(error: unknown): boolean => error === primary,
			);
		else await result;
		assert.deepEqual(killed, [0, 1]);
	}
});

await test("a failed or stalled browser kill preserves the primary failure and attempts every process", async (): Promise<void> => {
	const primary = new Error("Native capture did not finish.");
	const cleanup = new Error("Owned browser kill failed.");
	let attempts = 0;
	const processes = new BrowserProcesses(0);
	for (const stalled of [false, true])
		await processes.acquire(
			Promise.resolve({
				kill: (): Promise<void> => {
					attempts += 1;
					return stalled
						? new Promise<void>((): void => {})
						: Promise.reject(cleanup);
				},
			}),
		);
	await assert.rejects(
		usingBrowsers(processes, (): Promise<void> => Promise.reject(primary)),
		(error: unknown): boolean => {
			assert.ok(error instanceof AggregateError);
			assert.equal(error.cause, primary);
			assert.equal(error.errors.length, 3);
			assert.equal(error.errors[0], primary);
			assert.equal(error.errors[1], cleanup);
			const timeout: unknown = error.errors[2];
			assert.ok(timeout instanceof Error);
			assert.equal(timeout.name, "DeadlineError");
			return true;
		},
	);
	assert.equal(attempts, 2);
});

await test("a browser that finishes launching after its scope closes is killed before acquisition rejects", async (): Promise<void> => {
	const processes = new BrowserProcesses();
	let finish: (() => void) | undefined;
	let kills = 0;
	const launch = new Promise<{ kill: () => Promise<void> }>(
		(resolve: (process: { kill: () => Promise<void> }) => void): void => {
			finish = (): void => {
				resolve({
					kill: (): Promise<void> => {
						kills += 1;
						return Promise.resolve();
					},
				});
			};
		},
	);
	const acquiring = processes.acquire(launch);
	const primary = new Error("Launch wrapper exceeded its deadline.");
	await assert.rejects(
		usingBrowsers(processes, (): Promise<void> => Promise.reject(primary)),
		(error: unknown): boolean => error === primary,
	);
	assert.ok(finish);
	finish();
	await assert.rejects(
		acquiring,
		/The browser finished launching after its scope closed\./u,
	);
	assert.equal(kills, 1);
});
