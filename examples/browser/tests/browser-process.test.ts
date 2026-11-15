import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import { BrowserProcess } from "./browser-process.ts";

class Process extends EventEmitter {
	readonly pid: number = 23;
	exitCode: number | null = null;
	signalCode: NodeJS.Signals | null = null;

	finish(code: number | null, signal: NodeJS.Signals | null): void {
		this.exitCode = code;
		this.signalCode = signal;
		this.emit("exit", code, signal);
		this.emit("close", code, signal);
	}
}

await test("native browser diagnostics distinguish early exit from owned cleanup without exposing automation capabilities", async (): Promise<void> => {
	for (const early of [true, false]) {
		const process = new Process();
		let kills = 0;
		const observed = new BrowserProcess("Bob", {
			process: (): Process => process,
			wsEndpoint: (): string => "ws://private-automation-capability",
			kill: (): Promise<void> => {
				kills += 1;
				if (!early) process.finish(null, "SIGKILL");
				return Promise.resolve();
			},
		});
		assert.equal(observed.state().code, null);
		assert.equal(observed.state().exit, null);
		if (early) process.finish(17, null);
		await observed.kill();
		assert.equal(kills, 1);
		const state = observed.state();
		assert.equal(state.name, "Bob");
		assert.equal(state.pid, 23);
		assert.deepEqual(state.beforeCleanup, {
			code: early ? 17 : null,
			signal: null,
		});
		const event = {
			code: early ? 17 : null,
			signal: early ? null : "SIGKILL",
			phase: early ? "operation" : "cleanup",
		};
		assert.deepEqual(state.exit, event);
		assert.deepEqual(state.close, event);
		const serialized = JSON.stringify(state);
		assert.ok(serialized.length < 1024);
		assert.ok(!serialized.includes("private-automation-capability"));
		assert.equal(observed.wsEndpoint(), "ws://private-automation-capability");
	}
});
