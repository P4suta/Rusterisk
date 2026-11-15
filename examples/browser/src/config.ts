import { UserAgent } from "sip.js";
import type { ConnectionOptions } from "./phone.ts";

export interface AccountFields {
	readonly server: string;
	readonly address: string;
	readonly username: string;
	readonly password: string;
	readonly ice: string;
}

export class AccountFieldError extends Error {
	public readonly field: keyof AccountFields;
	public constructor(field: keyof AccountFields) {
		const messages: Readonly<Record<keyof AccountFields, string>> = {
			server:
				"WebSocket server URL: use wss:// remotely or ws:// on loopback, without credentials, a query, or a fragment.",
			address:
				"Account SIP address: enter a complete SIP address without a password.",
			username: "Authentication username: enter the username for your account.",
			password: "Password: enter the password for your account.",
			ice: "ICE servers: enter the JSON array supplied for your server.",
		};
		super(messages[field]);
		this.field = field;
	}
}

function fieldValue<T>(field: keyof AccountFields, read: () => T): T {
	try {
		return read();
	} catch {
		throw new AccountFieldError(field);
	}
}

function record(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function bounded(value: string, limit: number): boolean {
	return (
		value.length > 0 &&
		value.length <= limit &&
		!Array.from(value).some(
			(character: string): boolean =>
				character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127,
		)
	);
}

function iceUrl(value: unknown): value is string {
	return (
		typeof value === "string" &&
		bounded(value, 2048) &&
		/^(?:stun|stuns|turn|turns):[^\s]+$/u.test(value)
	);
}

function iceServers(source: string): RTCIceServer[] {
	if (!source.trim()) return [];
	if (source.length > 8192)
		throw new Error("ICE server configuration is too long.");
	const parsed: unknown = JSON.parse(source);
	if (!Array.isArray(parsed) || parsed.length > 8)
		throw new Error("Enter an array with at most eight ICE servers.");
	const result: RTCIceServer[] = [];
	const items: readonly unknown[] = parsed;
	for (const item of items) {
		if (
			!record(item) ||
			Object.keys(item).some(
				(key: string): boolean =>
					!["urls", "username", "credential"].includes(key),
			)
		)
			throw new Error(
				"ICE server fields must be urls, username, or credential.",
			);
		const urls: unknown = item.urls;
		const valid =
			iceUrl(urls) ||
			(Array.isArray(urls) &&
				urls.length > 0 &&
				urls.length <= 8 &&
				urls.every(iceUrl));
		if (!valid)
			throw new Error(
				"Use a stun, stuns, turn, or turns URL for each ICE server.",
			);
		if (typeof urls !== "string" && !Array.isArray(urls))
			throw new Error("Invalid ICE server URL.");
		const values: string[] = [];
		if (typeof urls === "string") values.push(urls);
		else {
			const candidates: readonly unknown[] = urls;
			for (const value of candidates) {
				if (!iceUrl(value)) throw new Error("Invalid ICE server URL.");
				values.push(value);
			}
		}
		const entry: RTCIceServer = { urls: values };
		const credentials: readonly ("username" | "credential")[] = [
			"username",
			"credential",
		];
		for (const key of credentials) {
			const value = item[key];
			if (value !== undefined) {
				if (typeof value !== "string" || !bounded(value, 512))
					throw new Error("ICE credentials must be bounded text.");
				entry[key] = value;
			}
		}
		result.push(entry);
	}
	return result;
}

export function destination(source: string): string {
	if (!bounded(source, 256) || /:[^@]*:/u.test(source.split("@")[0] ?? ""))
		throw new Error("Enter a SIP address without a password.");
	const uri = UserAgent.makeURI(source);
	if (!uri?.user || !["sip", "sips"].includes(uri.scheme))
		throw new Error("Enter a complete SIP address.");
	return source;
}

function serverUrl(source: string): string {
	if (!bounded(source, 2048)) throw new Error("Enter a WebSocket server URL.");
	const server = new URL(source);
	if (
		!["ws:", "wss:"].includes(server.protocol) ||
		server.username ||
		server.password ||
		server.hash ||
		server.search
	)
		throw new Error(
			"Use a WebSocket URL without credentials, a query, or a fragment.",
		);
	if (
		server.protocol === "ws:" &&
		!["localhost", "127.0.0.1", "[::1]"].includes(server.hostname)
	)
		throw new Error("Remote signaling requires a secure WebSocket URL.");
	return server.href;
}

export function connectionOptions(fields: AccountFields): ConnectionOptions {
	const server = fieldValue("server", (): string => serverUrl(fields.server));
	if (!bounded(fields.username, 128)) throw new AccountFieldError("username");
	if (!bounded(fields.password, 4096)) throw new AccountFieldError("password");
	return {
		server,
		address: fieldValue("address", (): string => destination(fields.address)),
		username: fields.username,
		password: fields.password,
		iceServers: fieldValue("ice", (): RTCIceServer[] => iceServers(fields.ice)),
	};
}
