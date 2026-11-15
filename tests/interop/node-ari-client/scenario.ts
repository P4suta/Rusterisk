import assert from "node:assert/strict";
import ari from "ari-client";
import type { ApplicationSnapshot, EndpointSnapshot } from "./contracts.ts";
import { application, endpoint, target } from "./contracts.ts";

type Snapshot = {
	readonly endpoints: readonly EndpointSnapshot[];
	readonly application: ApplicationSnapshot;
};

async function snapshot(address: string): Promise<Snapshot> {
	const client = await ari.connect(target(address), "app", "test-password");
	await client.start("compat-node");
	try {
		const resources: unknown = await client.endpoints.list();
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
			await client.applications.get({ applicationName: "compat-node" }),
		);
		return { endpoints, application: app };
	} finally {
		client.stop();
	}
}

const reference = target(process.env.RUSTERISK_REFERENCE_URL);
const actual = target(process.env.RUSTERISK_ACTUAL_URL);
assert.deepEqual(await snapshot(actual), await snapshot(reference));
console.log("Existing client resource comparison passed.");
