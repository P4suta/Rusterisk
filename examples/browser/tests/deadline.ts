export async function within<T>(
	phase: string,
	operation: Promise<T>,
	milliseconds: number,
): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		return await Promise.race([
			operation,
			new Promise<never>(
				(_resolve: unknown, reject: (error: Error) => void): void => {
					timer = setTimeout((): void => {
						const error = new Error(`${phase} exceeded its deadline.`);
						error.name = "DeadlineError";
						reject(error);
					}, milliseconds);
				},
			),
		]);
	} finally {
		clearTimeout(timer);
	}
}
