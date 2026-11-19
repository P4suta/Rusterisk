import assert from "node:assert/strict";

export interface CallChannels {
	readonly list: () => Promise<readonly string[]>;
	readonly hangup: (id: string) => Promise<void>;
	readonly requireEmpty: () => Promise<void>;
}

async function releaseChannels(channels: CallChannels): Promise<void> {
	const ids = await channels.list();
	const results = await Promise.allSettled(
		ids.map(async (id: string): Promise<void> => channels.hangup(id)),
	);
	const failures: unknown[] = [];
	for (const result of results)
		if (result.status === "rejected") failures.push(result.reason);
	if (failures.length > 0)
		throw new AggregateError(failures, "The browser call cleanup failed.");
	await channels.requireEmpty();
}

export async function usingCallChannels(
	channels: CallChannels,
	operation: () => Promise<void>,
): Promise<void> {
	assert.deepEqual(
		await channels.list(),
		[],
		"The server has active channels before the case.",
	);
	try {
		await operation();
	} catch (primary: unknown) {
		let cleanup: { error: unknown } | undefined;
		try {
			await releaseChannels(channels);
		} catch (error: unknown) {
			cleanup = { error };
		}
		if (cleanup)
			throw new AggregateError(
				[primary, cleanup.error],
				"The browser call and its channel cleanup failed.",
				{ cause: primary },
			);
		throw primary;
	}
}
