import assert from "node:assert/strict";
import {
	type Browser,
	expect,
	type Page,
	type TestInfo,
	test,
} from "@playwright/test";
import { checkBrowser } from "./browser.ts";
import { styleViolation } from "./styles.ts";

test.beforeEach(({ browser }: { browser: Browser }): void => {
	checkBrowser(browser);
});

function address(): string {
	const value = process.env.RUSTERISK_BROWSER_UI;
	assert.ok(value, "The owned browser fixture must provide its page URL.");
	return value;
}

async function reflow(page: Page): Promise<void> {
	const overflow = await page.evaluate((): string[] => {
		const problems: string[] = [];
		if (document.documentElement.scrollWidth > window.innerWidth + 1)
			problems.push("document");
		for (const element of document.querySelectorAll("main *")) {
			const bounds = element.getBoundingClientRect();
			if (
				bounds.width > 0 &&
				bounds.height > 0 &&
				(bounds.left < -1 || bounds.right > window.innerWidth + 1)
			)
				problems.push(element.id || element.tagName);
			for (const child of element.childNodes) {
				if (child.nodeType !== Node.TEXT_NODE || !child.textContent?.trim())
					continue;
				const range = document.createRange();
				range.selectNodeContents(child);
				for (const text of range.getClientRects())
					if (text.left < -1 || text.right > window.innerWidth + 1)
						problems.push(`${element.id || element.tagName}:text`);
			}
		}
		return problems;
	});
	assert.deepEqual(
		overflow,
		[],
		"Call controls and text must reflow within the viewport.",
	);
}

test("browser controls preserve monochrome reading, keyboard focus, targets, reflow, enlarged text, and spacing", async ({
	page,
}: { page: Page }, info: TestInfo): Promise<void> => {
	await page.goto(address());
	const declarations = await page.evaluate((): [string, string][] => {
		const values: [string, string][] = [];
		function inspect(rules: CSSRuleList, depth: number): void {
			if (depth > 8 || rules.length > 1000)
				throw new Error("Style inspection capacity exceeded.");
			for (const rule of rules) {
				if (rule instanceof CSSStyleRule) {
					for (const property of rule.style)
						values.push([property, rule.style.getPropertyValue(property)]);
				} else if (rule instanceof CSSGroupingRule)
					inspect(rule.cssRules, depth + 1);
				else throw new Error("An uninspected browser style rule was found.");
				if (values.length > 4096)
					throw new Error("Style declaration capacity exceeded.");
			}
		}
		if (document.styleSheets.length !== 1)
			throw new Error(
				"The browser fixture must load its maintained stylesheet.",
			);
		for (const sheet of document.styleSheets) inspect(sheet.cssRules, 0);
		return values;
	});
	assert.ok(declarations.length > 0);
	for (const [property, value] of declarations)
		assert.equal(
			styleViolation(property, value),
			undefined,
			`${property}: ${value}`,
		);
	await expect(page.locator("body")).toHaveCSS("font-size", "18px");
	await expect(page.locator("body")).toHaveCSS("line-height", "31.5px");
	for (const control of await page.locator("input, textarea, button").all()) {
		const bounds = await control.boundingBox();
		assert.ok(bounds && bounds.width >= 48 && bounds.height >= 48);
	}
	for (const id of [
		"server",
		"address",
		"username",
		"password",
		"ice",
		"connect",
	]) {
		await page.keyboard.press("Tab");
		await expect(page.locator(`#${id}`)).toBeFocused();
		await expect(page.locator(`#${id}`)).toHaveCSS("outline-width", "3px");
		await expect(page.locator(`#${id}`)).toHaveCSS(
			"outline-color",
			"rgb(0, 0, 0)",
		);
	}
	for (const width of [320, 768, 1280]) {
		await page.setViewportSize({ width, height: 900 });
		await reflow(page);
		await page.screenshot({
			path: info.outputPath(`browser-${String(width)}.png`),
			fullPage: true,
		});
	}
	await page.setViewportSize({ width: 900, height: 320 });
	await page.evaluate((): void => {
		window.scrollTo(0, 0);
	});
	await reflow(page);
	await expect(
		page.getByRole("heading", { name: "Browser calls" }),
	).toBeVisible();
	await page.screenshot({
		path: info.outputPath("browser-landscape.png"),
		fullPage: true,
	});
	await page.setViewportSize({ width: 320, height: 900 });
	await page.addStyleTag({ content: ":root { font-size: 225%; }" });
	await expect(page.locator("body")).toHaveCSS("font-size", "36px");
	await page.screenshot({
		path: info.outputPath("browser-enlarged.png"),
		fullPage: true,
	});
	await reflow(page);
	await page.addStyleTag({
		content:
			"* { line-height: 1.5 !important; letter-spacing: .12em !important; word-spacing: .16em !important; } p { margin-block-end: 2em !important; }",
	});
	await reflow(page);
	await page.screenshot({
		path: info.outputPath("browser-enlarged-spacing.png"),
		fullPage: true,
	});
	await page.emulateMedia({ forcedColors: "active" });
	await expect(
		page.getByRole("button", { name: "Connect", exact: true }),
	).toBeVisible();
	await reflow(page);
	await page.screenshot({
		path: info.outputPath("browser-forced-colors.png"),
		fullPage: true,
	});
});

test("disabled controls explain unavailable JavaScript and a failed application bundle", async ({
	browser,
}: {
	browser: import("@playwright/test").Browser;
}): Promise<void> => {
	const unavailable = await browser.newContext({ javaScriptEnabled: false });
	try {
		const page = await unavailable.newPage();
		await page.goto(address());
		await expect(page.locator("#startup-help")).toBeVisible();
		await expect(page.locator("#startup-help")).toContainText(
			"Enable JavaScript",
		);
		await expect(
			page.getByRole("button", { name: "Connect", exact: true }),
		).toBeDisabled();
	} finally {
		await unavailable.close();
	}
	const failed = await browser.newContext();
	try {
		const page = await failed.newPage();
		await page.route(
			"**/app.js",
			async (route: import("@playwright/test").Route): Promise<void> => {
				await route.abort("failed");
			},
		);
		await page.goto(address());
		await expect(
			page.getByRole("button", { name: "Connect", exact: true }),
		).toBeDisabled();
		await expect(page.locator("#startup-help")).toBeVisible();
	} finally {
		await failed.close();
	}
});
