import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { build } from "esbuild";

function normalized(text: string): string {
	return text.replace(/\s+/gu, " ").trim();
}

function sourcePath(url: URL): string {
	return fileURLToPath(url);
}

await test("the distributed browser bundle retains the pinned SIP.js copyright and permission notice", async (): Promise<void> => {
	const publisher = await readFile(
		new URL("../node_modules/sip.js/LICENSE.md", import.meta.url),
		"utf8",
	);
	assert.ok(publisher.startsWith("MIT License\n"));
	assert.ok(publisher.includes("Copyright (c) 2020 Junction Networks"));
	const result = await build({
		entryPoints: [sourcePath(new URL("../src/main.ts", import.meta.url))],
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

await test("browser bundle sources admit filesystem paths containing spaces", async (context: TestContext): Promise<void> => {
	const root = await mkdtemp(join(tmpdir(), "rusterisk browser "));
	context.after(
		(): Promise<void> => rm(root, { recursive: true, force: true }),
	);
	const source = join(root, "source.ts");
	await writeFile(source, "export const pathChecked: boolean = true;");
	const result = await build({
		entryPoints: [sourcePath(pathToFileURL(source))],
		bundle: true,
		format: "esm",
		target: "es2022",
		write: false,
		logLevel: "silent",
	});
	const bundle = result.outputFiles[0];
	assert.ok(bundle);
	assert.match(bundle.text, /pathChecked = true/u);
});
