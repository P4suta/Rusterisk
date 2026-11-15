export async function deadline<T>(
	stage: string,
	operation: PromiseLike<T>,
	milliseconds: number,
): Promise<T> {
	if (
		!Number.isSafeInteger(milliseconds) ||
		milliseconds < 1 ||
		milliseconds > 5000
	) {
		throw new Error("invalid client operation budget");
	}
	let timer: ReturnType<typeof setTimeout> | undefined;
	const expired = new Promise<never>(
		(
			_resolve: (value: never) => void,
			reject: (reason: Error) => void,
		): void => {
			timer = setTimeout((): void => {
				reject(new Error(`${stage} exceeded its deadline`));
			}, milliseconds);
		},
	);
	try {
		return await Promise.race([operation, expired]);
	} finally {
		clearTimeout(timer);
	}
}
