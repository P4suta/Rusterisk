import assert from "node:assert/strict";
import test from "node:test";
import { styleViolation } from "./styles.ts";

await test("the browser style gate rejects authored colors and effects while admitting black and white controls", (): void => {
	const refused: readonly (readonly [string, string])[] = [
		["color", "gray"],
		["background", "linear-gradient(black, white)"],
		["border-color", "#777"],
		["background-color", "rgba(0, 0, 0, 0.5)"],
		["color", "transparent"],
		["opacity", "0.5"],
		["box-shadow", "0 1px 2px black"],
		["text-shadow", "1px 1px white"],
		["filter", "blur(2px)"],
		["backdrop-filter", "blur(2px)"],
		["mix-blend-mode", "multiply"],
		["background-image", "url(hidden.png)"],
		["background-blend-mode", "screen"],
		["--accent", "gray"],
		["border", "2px solid red"],
		["animation-name", "fade"],
	];
	for (const [property, value] of refused)
		assert.ok(styleViolation(property, value), `${property}: ${value}`);
	const accepted: readonly (readonly [string, string])[] = [
		["color", "black"],
		["color", "rgb(255, 255, 255)"],
		["background-color", "#fff"],
		["background", "white"],
		["border", "2px solid black"],
		["outline", "3px solid currentColor"],
		["stroke", "currentColor"],
		["fill", "none"],
		["font-size", "1.125rem"],
		["line-height", "1.75"],
		["opacity", "1"],
		["box-shadow", "none"],
		["filter", "none"],
		["mix-blend-mode", "normal"],
	];
	for (const [property, value] of accepted)
		assert.equal(
			styleViolation(property, value),
			undefined,
			`${property}: ${value}`,
		);
});
