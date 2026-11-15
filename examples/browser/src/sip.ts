import { Registerer, RegistererState, UserAgent } from "sip.js";
import type { CallEvents, ConnectionOptions } from "./phone.ts";

export interface AudioOutput {
	attach(stream: MediaStream): Promise<void>;
	clear(): void;
}

interface Connection {
	readonly agent: UserAgent;
	readonly registration: Registerer;
	cancel: (() => void) | undefined;
	stopping: Promise<void> | undefined;
}

function registered(connection: Connection): Promise<void> {
	return new Promise(
		(resolve: () => void, reject: (error: Error) => void): void => {
			const timer = setTimeout((): void => {
				finish(new Error("Signaling registration timed out."));
			}, 8000);
			function finish(error?: Error): void {
				clearTimeout(timer);
				connection.registration.stateChange.removeListener(changed);
				connection.cancel = undefined;
				if (error) reject(error);
				else resolve();
			}
			function changed(state: RegistererState): void {
				switch (state) {
					case RegistererState.Registered:
						finish();
						break;
					case RegistererState.Unregistered:
					case RegistererState.Terminated:
						finish(new Error("Signaling registration was rejected."));
						break;
					case RegistererState.Initial:
						break;
				}
			}
			connection.cancel = (): void => {
				finish(new Error("Signaling registration was cancelled."));
			};
			connection.registration.stateChange.addListener(changed);
		},
	);
}

export class SipPhone {
	public readonly output: AudioOutput;
	private current: Connection | undefined;

	public constructor(output: AudioOutput) {
		this.output = output;
	}

	private async close(connection: Connection): Promise<void> {
		connection.cancel?.();
		connection.stopping ??= connection.agent.stop();
		await connection.stopping;
		if (this.current === connection) this.current = undefined;
		this.output.clear();
	}

	public async connect(
		options: ConnectionOptions,
		events: CallEvents,
	): Promise<void> {
		if (this.current)
			throw new Error("A signaling connection is already owned.");
		const uri = UserAgent.makeURI(options.address);
		if (!uri) throw new Error("Invalid signaling account address.");
		const agent = new UserAgent({
			uri,
			authorizationUsername: options.username,
			authorizationPassword: options.password,
			transportOptions: {
				server: options.server,
				connectionTimeout: 4,
				traceSip: false,
				keepAliveInterval: 0,
			},
			logBuiltinEnabled: false,
			logConfiguration: false,
			delegate: {
				onDisconnect: (): void => {
					events.disconnected();
				},
			},
		});
		const registration = new Registerer(agent, {
			expires: 120,
			logConfiguration: false,
		});
		const connection: Connection = {
			agent,
			registration,
			cancel: undefined,
			stopping: undefined,
		};
		this.current = connection;
		try {
			await agent.start();
			if (this.current !== connection || connection.stopping)
				throw new Error("Signaling connection was cancelled.");
			const accepted = registered(connection);
			const sent = registration.register({
				requestDelegate: {
					onReject: (): void => {
						connection.cancel?.();
					},
				},
			});
			await Promise.all([accepted, sent]);
		} catch {
			try {
				await this.close(connection);
			} catch {
				throw new Error(
					"Signaling registration and connection cleanup failed.",
				);
			}
			throw new Error("Signaling registration failed.");
		}
	}

	public async disconnect(): Promise<void> {
		const connection = this.current;
		if (connection) await this.close(connection);
	}
}
