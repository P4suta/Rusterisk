function color(value: string): boolean {
	return (
		[
			"black",
			"white",
			"#000",
			"#fff",
			"#000000",
			"#ffffff",
			"currentcolor",
		].includes(value.toLowerCase()) ||
		/^rgb\(\s*(?:0\s*[, ]\s*0\s*[, ]\s*0|255\s*[, ]\s*255\s*[, ]\s*255)\s*\)$/u.test(
			value,
		)
	);
}

export function styleViolation(
	property: string,
	value: string,
): string | undefined {
	const name = property.toLowerCase();
	const source = value.trim();
	const prohibited = (): string => `${property}: ${value}`;
	if (name.startsWith("--") || /gradient\(/iu.test(source)) return prohibited();
	if (
		["color", "fill", "stroke", "caret-color"].includes(name) ||
		name.endsWith("-color")
	)
		return color(source) ||
			(["fill", "stroke"].includes(name) && source === "none")
			? undefined
			: prohibited();
	if (name === "background") return color(source) ? undefined : prohibited();
	if (
		name === "border" ||
		name === "outline" ||
		/^border-(?:top|right|bottom|left|block|inline)(?:-(?:start|end))?$/u.test(
			name,
		)
	) {
		const parts = source.split(/\s+/u);
		return parts.every(
			(part: string): boolean =>
				color(part) ||
				/^(?:\d+(?:\.\d+)?(?:px|em|rem)|0|none|solid|dashed|dotted|double)$/u.test(
					part,
				),
		)
			? undefined
			: prohibited();
	}
	if (name === "opacity") return source === "1" ? undefined : prohibited();
	if (
		[
			"box-shadow",
			"text-shadow",
			"filter",
			"backdrop-filter",
			"background-image",
			"mask-image",
			"animation-name",
		].includes(name)
	)
		return source === "none" ? undefined : prohibited();
	if (["mix-blend-mode", "background-blend-mode"].includes(name))
		return source === "normal" ? undefined : prohibited();
	return undefined;
}
