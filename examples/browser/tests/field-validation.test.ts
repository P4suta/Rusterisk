import assert from "node:assert/strict";
import test from "node:test";
import { AccountFieldError, type AccountFields } from "../src/config.ts";
import {
	AccountValidation,
	type ValidityControl,
} from "../src/field-validation.ts";

class Control implements ValidityControl {
	public readonly validity: { customError: boolean } = { customError: false };
	public message: string = "";
	public reports: number = 0;
	private readonly attributes: Map<string, string> = new Map();
	private input: (() => void) | undefined;
	public setCustomValidity(message: string): void {
		this.message = message;
		this.validity.customError = message.length > 0;
	}
	public getAttribute(name: string): string | null {
		return this.attributes.get(name) ?? null;
	}
	public setAttribute(name: string, value: string): void {
		this.attributes.set(name, value);
	}
	public removeAttribute(name: string): void {
		this.attributes.delete(name);
	}
	public addEventListener(_type: "input", action: () => void): void {
		assert.equal(this.input, undefined);
		this.input = action;
	}
	public reportValidity(): boolean {
		this.reports += 1;
		return !this.validity.customError;
	}
	public edit(): void {
		this.input?.();
	}
}

await test("field validation identifies only the rejected control and clears native custom validity when editing resumes", (): void => {
	const fields = {
		server: new Control(),
		address: new Control(),
		username: new Control(),
		password: new Control(),
		ice: new Control(),
	};
	fields.server.setAttribute("aria-describedby", "server-help");
	fields.address.setAttribute("aria-describedby", "address-help");
	let cleared = 0;
	const validation = new AccountValidation(fields, (): void => {
		cleared += 1;
	});
	const names: readonly (keyof AccountFields)[] = [
		"server",
		"address",
		"username",
		"password",
		"ice",
	];
	for (const name of names) {
		const field = fields[name];
		const description = field.getAttribute("aria-describedby");
		field.edit();
		assert.equal(field.getAttribute("aria-invalid"), null);
		const error = new AccountFieldError(name);
		validation.reject(error);
		validation.reject(error);
		assert.equal(field.validity.customError, true);
		assert.equal(field.message, error.message);
		assert.equal(field.reports, 2);
		assert.equal(field.getAttribute("aria-invalid"), "true");
		assert.equal(
			field.getAttribute("aria-describedby"),
			description ? `${description} connection-error` : "connection-error",
		);
		for (const other of names.filter(
			(value: keyof AccountFields): boolean => value !== name,
		))
			assert.equal(fields[other].validity.customError, false);
		field.edit();
		assert.equal(field.validity.customError, false);
		assert.equal(field.message, "");
		assert.equal(field.getAttribute("aria-invalid"), null);
		assert.equal(field.getAttribute("aria-describedby"), description);
	}
	assert.equal(cleared, 5);
});
