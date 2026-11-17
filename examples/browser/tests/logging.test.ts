import assert from "node:assert/strict";
import { realpathSync } from "node:fs";
import { createRequire } from "node:module";
import { resolve } from "node:path";
import test from "node:test";
import { ESLint, type Linter } from "eslint";

type Operation = (...arguments_: readonly unknown[]) => unknown;

const browserRoot = resolve(import.meta.dirname, "..");
const fixtureRequire = createRequire(import.meta.url);
const parserRequire = createRequire(
	fixtureRequire.resolve("@typescript-eslint/typescript-estree"),
);
const unsupported =
	"Explicit project lists are unsupported; use projectService: true.";

function isOperation(value: unknown): value is Operation {
	return typeof value === "function";
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function adapterOperation(name: string): Operation {
	const adapter: unknown = parserRequire("tinyglobby");
	assert.ok(isRecord(adapter));
	const operation: unknown = adapter[name];
	assert.ok(isOperation(operation));
	return operation;
}

await test("production logging options reject enabled, indirect, and computed sensitive output", async (): Promise<void> => {
	const eslint = new ESLint({ cwd: `${import.meta.dirname}/..` });
	for (const property of [
		"logBuiltinEnabled: true",
		"logConfiguration: true",
		"traceSip: true",
		'"traceSip": true',
		'["logBuiltinEnabled"]: true',
		"traceSip: enabled",
		"logConfiguration",
		'logBuiltinEnabled: "false"',
		"traceSip: 0",
	]) {
		const result = await eslint.lintText(
			`export const settings = { ${property} };`,
			{ filePath: "src/sip.ts" },
		);
		assert.equal(
			result.flatMap((file: ESLint.LintResult): readonly string[] =>
				file.messages.flatMap(
					(message: Linter.LintMessage): readonly string[] =>
						message.ruleId === "no-restricted-syntax" ? [message.message] : [],
				),
			).length,
			1,
			property,
		);
	}
	const accepted = await eslint.lintText(
		"export const settings = { logBuiltinEnabled: false, logConfiguration: false, traceSip: false };",
		{ filePath: "src/sip.ts" },
	);
	assert.equal(
		accepted.flatMap(
			(file: ESLint.LintResult): readonly unknown[] => file.messages,
		).length,
		0,
	);
	const output = await eslint.lintText('console.log("sensitive payload");', {
		filePath: "src/sip.ts",
	});
	assert.equal(
		output.flatMap((file: ESLint.LintResult): readonly string[] =>
			file.messages.flatMap((message: Linter.LintMessage): readonly string[] =>
				message.ruleId === "no-console" ? [message.message] : [],
			),
		).length,
		1,
	);
});

await test("the browser parser resolves the single shared project service adapter", (): void => {
	assert.equal(
		realpathSync(parserRequire.resolve("tinyglobby")),
		resolve(
			browserRoot,
			"../../tests/interop/node-ari-client/project-service-only/index.mjs",
		),
	);
});

await test("the browser adapter rejects every explicit project resolution operation", (): void => {
	for (const name of ["isDynamicPattern", "globSync"]) {
		const operation = adapterOperation(name);
		for (const pattern of ["tsconfig.json", "*.ts", "+(*)"]) {
			assert.throws(
				(): unknown =>
					operation(pattern, {
						cwd: browserRoot,
						expandDirectories: false,
						ignore: ["**/node_modules/**"],
					}),
				{ name: "Error", message: unsupported },
			);
		}
	}
});

await test("the browser keeps project service and type-aware rejection", async (): Promise<void> => {
	const lint = new ESLint({ cwd: browserRoot });
	for (const file of [
		"src/sip.ts",
		"tests/logging.test.ts",
		"eslint.config.mjs",
	]) {
		const configuration: unknown = await lint.calculateConfigForFile(file);
		assert.ok(isRecord(configuration));
		const language = configuration.languageOptions;
		assert.ok(isRecord(language));
		const options = language.parserOptions;
		assert.ok(isRecord(options));
		assert.equal(options.projectService, true);
		assert.equal(options.project, undefined);
		assert.equal(options.programs, undefined);
	}
	const rejected = await lint.lintText(
		'export function invalid(): string { return JSON.parse("0"); }',
		{ filePath: "src/sip.ts" },
	);
	assert.ok(
		rejected.some((result: ESLint.LintResult): boolean =>
			result.messages.some(
				(message: Linter.LintMessage): boolean =>
					message.ruleId === "@typescript-eslint/no-unsafe-return",
			),
		),
	);
	const accepted = await lint.lintText(
		"export function valid(value: string): string { return value; }",
		{ filePath: "src/sip.ts" },
	);
	assert.ok(
		accepted.every(
			(result: ESLint.LintResult): boolean =>
				result.errorCount === 0 && result.warningCount === 0,
		),
	);
});

await test("the browser rejects explicit project lists instead of changing typed linting", async (): Promise<void> => {
	const lint = new ESLint({
		cwd: browserRoot,
		overrideConfig: {
			languageOptions: {
				parserOptions: {
					projectService: false,
					project: ["./tsconfig.json"],
				},
			},
		},
	});
	const results = await lint.lintText(
		"export function valid(value: string): string { return value; }",
		{ filePath: "src/sip.ts" },
	);
	assert.ok(
		results.some((result: ESLint.LintResult): boolean =>
			result.messages.some(
				(message: Linter.LintMessage): boolean =>
					message.fatal === true && message.message.includes(unsupported),
			),
		),
	);
});
