import js from "@eslint/js";
import { defineConfig } from "eslint/config";
import tseslint from "typescript-eslint";

export default defineConfig(
	{
		ignores: [
			"dist/**",
			"node_modules/**",
			"test-results/**",
			"playwright-report/**",
		],
	},
	js.configs.recommended,
	tseslint.configs.strictTypeChecked,
	{
		files: ["**/*.ts", "*.mjs"],
		linterOptions: {
			noInlineConfig: true,
			reportUnusedDisableDirectives: "error",
		},
		languageOptions: {
			parserOptions: {
				projectService: true,
				tsconfigRootDir: import.meta.dirname,
			},
		},
		rules: {
			"@typescript-eslint/explicit-function-return-type": "error",
			"@typescript-eslint/explicit-module-boundary-types": "error",
			"@typescript-eslint/consistent-type-assertions": [
				"error",
				{ assertionStyle: "never" },
			],
			"@typescript-eslint/no-non-null-assertion": "error",
			"@typescript-eslint/typedef": [
				"error",
				{
					parameter: true,
					arrowParameter: true,
					memberVariableDeclaration: true,
					propertyDeclaration: true,
				},
			],
			"@typescript-eslint/ban-ts-comment": [
				"error",
				{
					"ts-expect-error": true,
					"ts-ignore": true,
					"ts-nocheck": true,
					"ts-check": false,
				},
			],
		},
	},
	{
		files: ["src/**/*.ts"],
		rules: {
			"no-console": "error",
			"no-restricted-syntax": [
				"error",
				{
					selector:
						"Property:is([key.name=/^(logBuiltinEnabled|logConfiguration|traceSip)$/],[key.value=/^(logBuiltinEnabled|logConfiguration|traceSip)$/]):not([value.type='Literal'][value.raw='false'])",
					message: "Sensitive signaling logging must be explicitly disabled.",
				},
			],
		},
	},
);
