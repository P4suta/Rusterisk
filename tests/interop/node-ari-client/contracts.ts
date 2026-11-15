export type EndpointSnapshot = {
	readonly technology: string;
	readonly resource: string;
	readonly state: "unknown" | "offline" | "online" | undefined;
	readonly channel_ids: readonly string[];
};

export type ApplicationSnapshot = {
	readonly name: string;
	readonly channel_ids: readonly string[];
	readonly bridge_ids: readonly string[];
	readonly endpoint_ids: readonly string[];
	readonly device_names: readonly string[];
};

function strings(value: unknown): readonly string[] {
	if (!Array.isArray(value) || value.length > 256) {
		throw new Error("invalid resource membership list");
	}
	const values: unknown[] = value;
	return values.map((item: unknown): string => {
		if (typeof item !== "string" || item.length < 1 || item.length > 256) {
			throw new Error("invalid resource membership");
		}
		return item;
	});
}

export function application(value: unknown): ApplicationSnapshot {
	if (
		typeof value !== "object" ||
		value === null ||
		!("name" in value) ||
		!("channel_ids" in value) ||
		!("bridge_ids" in value) ||
		!("endpoint_ids" in value) ||
		!("device_names" in value)
	) {
		throw new Error("invalid app snapshot");
	}
	return {
		name: identifier(value.name),
		channel_ids: strings(value.channel_ids),
		bridge_ids: strings(value.bridge_ids),
		endpoint_ids: strings(value.endpoint_ids),
		device_names: strings(value.device_names),
	};
}

function identifier(value: unknown): string {
	if (
		typeof value !== "string" ||
		value.length < 1 ||
		value.length > 128 ||
		value === "." ||
		value === ".." ||
		!/^[A-Za-z0-9._-]+$/.test(value)
	) {
		throw new Error("invalid resource identifier");
	}
	return value;
}

export function endpoint(value: unknown): EndpointSnapshot {
	if (
		typeof value !== "object" ||
		value === null ||
		!("technology" in value) ||
		!("resource" in value) ||
		!("channel_ids" in value) ||
		value.technology !== "PJSIP" ||
		!Array.isArray(value.channel_ids) ||
		value.channel_ids.length > 64
	) {
		throw new Error("invalid endpoint snapshot");
	}
	const state = "state" in value ? value.state : undefined;
	if (
		state !== undefined &&
		state !== "unknown" &&
		state !== "offline" &&
		state !== "online"
	) {
		throw new Error("invalid endpoint state");
	}
	const channels: unknown[] = value.channel_ids;
	return {
		technology: value.technology,
		resource: identifier(value.resource),
		state,
		channel_ids: channels.map(identifier),
	};
}

export function target(value: string | undefined): string {
	if (value === undefined || value.length > 4096) {
		throw new Error("local management address is required");
	}
	const address = new URL(value);
	if (
		address.protocol !== "http:" ||
		!["127.0.0.1", "localhost", "[::1]"].includes(address.hostname) ||
		address.port === "0" ||
		address.username !== "" ||
		address.password !== "" ||
		address.pathname !== "/" ||
		address.search !== "" ||
		address.hash !== ""
	) {
		throw new Error("invalid local management address");
	}
	return address.origin;
}
