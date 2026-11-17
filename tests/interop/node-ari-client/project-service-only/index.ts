export function isDynamicPattern(): never {
	throw new Error(
		"Explicit project lists are unsupported; use projectService: true.",
	);
}

export function globSync(): never {
	throw new Error(
		"Explicit project lists are unsupported; use projectService: true.",
	);
}
