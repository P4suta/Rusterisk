import { defineConfig } from "@playwright/test";
import { browserChannel } from "./tests/browser.ts";

export default defineConfig({
	testDir: "./tests",
	testMatch: "**/*.spec.ts",
	fullyParallel: false,
	forbidOnly: true,
	retries: 0,
	workers: 1,
	timeout: 60000,
	globalTimeout: 120000,
	expect: { timeout: 8000 },
	outputDir: process.env.RUSTERISK_BROWSER_ARTIFACTS ?? "test-results",
	reporter: [
		["list"],
		[
			"json",
			{
				outputFile:
					process.env.PLAYWRIGHT_JSON_OUTPUT_FILE ??
					"test-results/results.json",
			},
		],
	],
	use: {
		browserName: "chromium",
		channel: browserChannel,
		headless: true,
		trace: "off",
		video: "off",
		screenshot: "off",
	},
});
