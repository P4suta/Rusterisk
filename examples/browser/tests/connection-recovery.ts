import assert from "node:assert/strict";
import { expect, type Locator, type Page } from "@playwright/test";
import { captureAttempts } from "./capture-observer.ts";

export async function verifyConnectionRecovery(page: Page): Promise<void> {
	const captures = await captureAttempts(page);
	const password = page.getByLabel("Password", { exact: true });
	const connect = page.getByRole("button", { name: "Connect", exact: true });
	const rejected: readonly (readonly [
		Locator,
		string,
		string,
		string,
		string,
	])[] = [
		[
			page.getByLabel("Account SIP address"),
			"not a URI",
			"sip:alice@localhost",
			"Account SIP address:",
			"address-help",
		],
		[
			page.getByLabel("ICE servers, optional"),
			"{",
			"[]",
			"ICE servers:",
			"ice-help",
		],
	];
	for (const [field, invalid, corrected, label, help] of rejected) {
		await field.fill(invalid);
		await connect.press("Enter");
		await expect(page.locator("#connection-error")).toContainText(label);
		await expect(field).toHaveAttribute("aria-invalid", "true");
		await expect(field).toHaveAttribute(
			"aria-describedby",
			/connection-error/u,
		);
		await expect(field).toBeFocused();
		await field.fill(corrected);
		await expect(field).not.toHaveAttribute("aria-invalid", "true");
		await expect(field).toHaveAttribute("aria-describedby", help);
		await expect(page.locator("#connection-error")).toBeHidden();
	}
	await password.fill("incorrect-test-password");
	await connect.press("Enter");
	await expect(page.locator("#connection-error")).toContainText(
		"Connection failed.",
	);
	await expect(page.locator("#connection-status")).toHaveText("Disconnected");
	await expect(connect).toBeEnabled();
	await expect(page.locator("#call-error")).toBeHidden();
	await expect(page.getByLabel("Account SIP address")).toHaveValue(
		"sip:alice@localhost",
	);
	await password.fill("test-password");
	await connect.press("Enter");
	await expect(page.locator("#connection-status")).toHaveText(
		"Connected and registered",
	);
	await expect(page.locator("#connection-error")).toBeHidden();
	assert.equal(await captureAttempts(page), captures);
	await page
		.getByRole("button", { name: "Disconnect", exact: true })
		.press("Enter");
	await expect(page.locator("#connection-status")).toHaveText("Disconnected");
	await expect(page.locator("#call-status")).toHaveText("No active call");
}
