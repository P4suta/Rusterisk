import assert from "node:assert/strict";
import ari from "ari-client";
import type { ApplicationSnapshot, EndpointSnapshot } from "./contracts.ts";
import { application, endpoint, target } from "./contracts.ts";
import { deadline } from "./deadline.ts";

type Snapshot = {
	readonly endpoints: readonly EndpointSnapshot[];
	readonly application: ApplicationSnapshot;
	readonly filtered: ApplicationSnapshot;
	readonly cleared: ApplicationSnapshot;
};

async function snapshot(address: string): Promise<Snapshot> {
	const client = await deadline(
		`discovery at ${target(address)}`,
		ari.connect(target(address), "app", "test-password"),
		5000,
	);
	await deadline("event subscription", client.start("compat-node"), 5000);
	try {
		const resources: unknown = await deadline(
			"endpoint list",
			client.endpoints.list(),
			5000,
		);
		if (!Array.isArray(resources) || resources.length > 256) {
			throw new Error("invalid endpoint list");
		}
		const values: unknown[] = resources;
		const endpoints = values
			.map(endpoint)
			.sort((left: EndpointSnapshot, right: EndpointSnapshot): number => {
				return left.resource < right.resource
					? -1
					: left.resource > right.resource
						? 1
						: 0;
			});
		const app = application(
			await deadline(
				"app query",
				client.applications.get({ applicationName: "compat-node" }),
				5000,
			),
		);
		const filtered = application(
			await deadline(
				"event filter",
				client.applications.filter({
					applicationName: "compat-node",
					filter: {
						allowed: [{ type: "StasisStart" }],
						disallowed: [{ type: "ChannelVarset" }],
					},
				}),
				5000,
			),
		);
		const cleared = application(
			await deadline(
				"filter reset",
				client.applications.filter({
					applicationName: "compat-node",
					filter: {},
				}),
				5000,
			),
		);
		return { endpoints, application: app, filtered, cleared };
	} finally {
		client.stop();
	}
}

const reference = target(process.env.RUSTERISK_REFERENCE_URL);
const actual = target(process.env.RUSTERISK_ACTUAL_URL);
for (let attempt = 0; attempt < 5; attempt += 1) {
	assert.deepEqual(await snapshot(actual), await snapshot(reference));
}
console.log("Existing client resource comparison passed.");
