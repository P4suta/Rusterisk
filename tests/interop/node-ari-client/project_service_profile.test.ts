import assert from "node:assert/strict";
import { realpathSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import test from "node:test";
import type { Linter } from "eslint";
import { ESLint } from "eslint";

type Operation = (...arguments_: readonly unknown[]) => unknown;

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

await test("the parser resolves its glob dependency to the owned adapter", (): void => {
	assert.equal(
		realpathSync(parserRequire.resolve("tinyglobby")),
		join(import.meta.dirname, "project-service-only", "index.mjs"),
	);
});

await test("the adapter rejects every explicit project resolution operation", (): void => {
	for (const name of ["isDynamicPattern", "globSync"]) {
		const operation = adapterOperation(name);
		for (const pattern of ["tsconfig.json", "*.ts", "+(*)"]) {
			assert.throws(
				(): unknown =>
					operation(pattern, {
						cwd: import.meta.dirname,
						expandDirectories: false,
						ignore: ["**/node_modules/**"],
					}),
				{ name: "Error", message: unsupported },
			);
		}
	}
});

await test("the actual fixture keeps project service and type-aware rejection", async (): Promise<void> => {
	const lint = new ESLint({ cwd: import.meta.dirname });
	for (const file of ["contracts.ts", "scenario.ts", "eslint.config.mjs"]) {
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
		{ filePath: join(import.meta.dirname, "contracts.ts") },
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
		{ filePath: join(import.meta.dirname, "contracts.ts") },
	);
	assert.ok(
		accepted.every(
			(result: ESLint.LintResult): boolean =>
				result.errorCount === 0 && result.warningCount === 0,
		),
	);
});

await test("an explicit project list fails instead of silently changing typed linting", async (): Promise<void> => {
	const lint = new ESLint({
		cwd: import.meta.dirname,
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
		{ filePath: join(import.meta.dirname, "contracts.ts") },
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
