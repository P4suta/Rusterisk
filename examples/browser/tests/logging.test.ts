import assert from "node:assert/strict";
import test from "node:test";
import { ESLint, type Linter } from "eslint";

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
