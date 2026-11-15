import type { AccountFieldError, AccountFields } from "./config.ts";

export interface ValidityControl {
	readonly validity: { readonly customError: boolean };
	setCustomValidity(message: string): void;
	getAttribute(name: string): string | null;
	setAttribute(name: string, value: string): void;
	removeAttribute(name: string): void;
	addEventListener(type: "input", action: () => void): void;
	reportValidity(): boolean;
}

export class AccountValidation {
	private readonly fields: Readonly<
		Record<keyof AccountFields, ValidityControl>
	>;
	public constructor(
		fields: Readonly<Record<keyof AccountFields, ValidityControl>>,
		cleared: () => void,
	) {
		this.fields = fields;
		for (const field of Object.values(fields))
			field.addEventListener("input", (): void => {
				if (!field.validity.customError) return;
				field.setCustomValidity("");
				field.removeAttribute("aria-invalid");
				const references = descriptions(field).filter(
					(reference: string): boolean => reference !== "connection-error",
				);
				if (references.length > 0)
					field.setAttribute("aria-describedby", references.join(" "));
				else field.removeAttribute("aria-describedby");
				cleared();
			});
	}
	public reject(error: AccountFieldError): void {
		const field = this.fields[error.field];
		field.setCustomValidity(error.message);
		field.setAttribute("aria-invalid", "true");
		field.setAttribute(
			"aria-describedby",
			Array.from(new Set([...descriptions(field), "connection-error"])).join(
				" ",
			),
		);
		field.reportValidity();
	}
}

function descriptions(field: ValidityControl): string[] {
	return (field.getAttribute("aria-describedby") ?? "")
		.split(/\s+/u)
		.filter((reference: string): boolean => reference.length > 0);
}
