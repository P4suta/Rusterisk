import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { build } from "esbuild";

function normalized(text: string): string {
	return text.replace(/\s+/gu, " ").trim();
}

await test("the distributed browser bundle retains the pinned SIP.js copyright and permission notice", async (): Promise<void> => {
	const publisher = await readFile(
		new URL("../node_modules/sip.js/LICENSE.md", import.meta.url),
		"utf8",
	);
	assert.ok(publisher.startsWith("MIT License\n"));
	assert.ok(publisher.includes("Copyright (c) 2020 Junction Networks"));
	const result = await build({
		entryPoints: [new URL("../src/main.ts", import.meta.url).pathname],
		bundle: true,
		format: "esm",
		target: "es2022",
		write: false,
	});
	assert.equal(result.outputFiles.length, 1);
	const bundle = result.outputFiles[0];
	assert.ok(bundle);
	assert.ok(
		normalized(bundle.text).includes(normalized(publisher)),
		"The complete publisher license must travel with app.js.",
	);
});
