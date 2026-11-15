import assert from "node:assert/strict";
import test from "node:test";
import { receivedTone } from "./audio.ts";

function tone(frequency: number, scale: number = 0.2): readonly number[] {
	return Array.from(
		{ length: 4800 },
		(_sample: unknown, index: number): number =>
			scale * Math.sin((2 * Math.PI * frequency * index) / 48000),
	);
}

await test("received audio must contain the expected tone and reject silence, the other sender, and invalid samples", (): void => {
	assert.equal(receivedTone(tone(440), 48000, 440, 660), true);
	assert.equal(receivedTone(tone(660), 48000, 660, 440), true);
	assert.equal(receivedTone(tone(660), 48000, 440, 660), false);
	assert.equal(receivedTone(tone(440, 0), 48000, 440, 660), false);
	assert.equal(receivedTone(tone(440, 0.00001), 48000, 440, 660), false);
	assert.equal(
		receivedTone([Number.NaN, ...tone(440)], 48000, 440, 660),
		false,
	);
	assert.equal(receivedTone([], 48000, 440, 660), false);
	assert.equal(receivedTone(tone(440), 0, 440, 660), false);
	assert.equal(receivedTone(tone(440), 48000, 24000, 660), false);
	const second = tone(660);
	const mixed = tone(440).map((sample: number, index: number): number => {
		const other = second[index];
		assert.ok(other !== undefined);
		return sample + other;
	});
	assert.equal(receivedTone(mixed, 48000, 440, 660), false);
});
