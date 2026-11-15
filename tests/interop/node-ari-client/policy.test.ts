import assert from "node:assert/strict";
import { join } from "node:path";
import test from "node:test";
import type { Linter } from "eslint";
import { ESLint } from "eslint";
import ts from "typescript";

await test("the actual lint configuration rejects missing annotations and unchecked types", async (): Promise<void> => {
	const lint = new ESLint({ cwd: import.meta.dirname });
	const fixtures: readonly (readonly [string, string])[] = [
		[
			"export function invalid(value: string) { return value; }",
			"@typescript-eslint/explicit-function-return-type",
		],
		[
			"export function invalid(value: any): any { return value; }",
			"@typescript-eslint/no-explicit-any",
		],
		[
			"export function invalid(value: unknown): string { return value as string; }",
			"@typescript-eslint/consistent-type-assertions",
		],
	];
	for (const [source, rule] of fixtures) {
		const results = await lint.lintText(source, {
			filePath: join(import.meta.dirname, "contracts.ts"),
		});
		assert.ok(
			results.some((result: ESLint.LintResult): boolean =>
				result.messages.some(
					(message: Linter.LintMessage): boolean => message.ruleId === rule,
				),
			),
		);
	}
	const valid = await lint.lintText(
		"export function valid(value: string): string { return value; }",
		{ filePath: join(import.meta.dirname, "contracts.ts") },
	);
	assert.ok(
		valid.every(
			(result: ESLint.LintResult): boolean =>
				result.errorCount === 0 && result.warningCount === 0,
		),
	);
});

await test("the actual compiler configuration rejects an incompatible return value", (): void => {
	const configuration = ts.getParsedCommandLineOfConfigFile(
		join(import.meta.dirname, "tsconfig.json"),
		{},
		{
			...ts.sys,
			onUnRecoverableConfigFileDiagnostic(diagnostic: ts.Diagnostic): void {
				throw new Error(
					ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n"),
				);
			},
		},
	);
	assert.ok(configuration);
	assert.deepEqual(configuration.errors, []);
	const name = join(import.meta.dirname, "policy-return.ts");
	const host = ts.createCompilerHost(configuration.options);
	const original = host.getSourceFile.bind(host);
	host.getSourceFile = (
		fileName: string,
		languageVersion: ts.ScriptTarget | ts.CreateSourceFileOptions,
		onError?: (message: string) => void,
		shouldCreateNewSourceFile?: boolean,
	): ts.SourceFile | undefined => {
		return fileName === name
			? ts.createSourceFile(
					name,
					"export function invalid(value: number): string { return value; }",
					languageVersion,
					true,
				)
			: original(fileName, languageVersion, onError, shouldCreateNewSourceFile);
	};
	const program = ts.createProgram([name], configuration.options, host);
	assert.ok(
		ts
			.getPreEmitDiagnostics(program)
			.some((diagnostic: ts.Diagnostic): boolean => diagnostic.code === 2322),
	);
});
