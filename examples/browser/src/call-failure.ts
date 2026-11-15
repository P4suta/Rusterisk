export async function releaseFailedCall(
	primary: unknown,
	dispose: () => Promise<void>,
	release: () => void,
): Promise<never> {
	const failures: unknown[] = [];
	try {
		await dispose();
	} catch (error: unknown) {
		failures.push(error);
	}
	try {
		release();
	} catch (error: unknown) {
		failures.push(error);
	}
	if (failures.length > 0)
		throw new AggregateError(
			[primary, ...failures],
			"The outgoing call and session cleanup failed.",
			{ cause: primary },
		);
	throw new Error("The outgoing call failed.", { cause: primary });
}
