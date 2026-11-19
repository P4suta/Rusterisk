import {
	type Invitation,
	Inviter,
	Registerer,
	RegistererState,
	type Session,
	UserAgent,
	type Web,
} from "sip.js";
import { releaseFailedCall } from "./call-failure.ts";
import type { MediaCapture } from "./capture.ts";
import type { CallEvents, ConnectionOptions, SessionEvents } from "./phone.ts";
import { OwnedSession } from "./session.ts";

export interface AudioOutput {
	attach(stream: MediaStream): Promise<void>;
	clear(): void;
}

interface Connection {
	readonly agent: UserAgent;
	readonly registration: Registerer;
	readonly events: CallEvents;
	readonly jobs: Set<Promise<void>>;
	failed: boolean;
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
	private callSession: OwnedSession | undefined;

	public constructor(output: AudioOutput) {
		this.output = output;
	}

	private stop(connection: Connection): void {
		connection.cancel?.();
		connection.stopping ??= connection.agent
			.stop()
			.catch((): void => {
				connection.failed = true;
			})
			.finally((): void => {
				if (this.current === connection) {
					this.callSession?.release();
					this.current = undefined;
				}
			});
	}

	private background(
		connection: Connection,
		operation: () => Promise<void>,
	): void {
		if (connection.jobs.size >= 4 || connection.stopping) {
			connection.failed = true;
			connection.events.disconnected();
			this.stop(connection);
			return;
		}
		const job = operation()
			.catch((): void => {
				connection.failed = true;
				connection.events.disconnected();
				this.stop(connection);
			})
			.finally((): void => {
				connection.jobs.delete(job);
			});
		connection.jobs.add(job);
	}

	private own(
		connection: Connection,
		session: Invitation | Inviter,
		capture: MediaCapture | undefined,
		events: SessionEvents,
	): OwnedSession {
		const owned = new OwnedSession(
			session,
			capture,
			events,
			this.output,
			(): void => {
				if (this.callSession === owned) {
					this.callSession = undefined;
					this.output.clear();
				}
			},
			(operation: () => Promise<void>): void => {
				this.background(connection, operation);
			},
		);
		this.callSession = owned;
		return owned;
	}

	private async close(connection: Connection): Promise<void> {
		this.stop(connection);
		await connection.stopping;
		await Promise.all(connection.jobs);
		if (connection.failed)
			throw new Error("Signaling connection cleanup failed.");
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
			sessionDescriptionHandlerFactory: (
				session: Session,
			): Web.SessionDescriptionHandler => {
				const owned = this.callSession;
				if (
					!owned ||
					owned.session !== session ||
					this.current !== connection ||
					connection.stopping
				)
					throw new Error("A current call session is required.");
				return owned.description(options.iceServers);
			},
			delegate: {
				onDisconnect: (): void => {
					if (this.current === connection && !connection.stopping) {
						events.disconnected();
						this.stop(connection);
					}
				},
				onInvite: (invitation: Invitation): void => {
					const incoming =
						this.current === connection &&
						!connection.stopping &&
						!this.callSession
							? events.incoming(
									invitation.remoteIdentity.displayName ||
										invitation.remoteIdentity.uri.user ||
										"",
								)
							: undefined;
					if (incoming) this.own(connection, invitation, undefined, incoming);
					else
						this.background(
							connection,
							(): Promise<void> => invitation.reject({ statusCode: 486 }),
						);
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
			events,
			jobs: new Set(),
			failed: false,
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

	public async call(
		destination: string,
		capture: MediaCapture,
		events: SessionEvents,
	): Promise<void> {
		const connection = this.current;
		if (
			!connection ||
			connection.stopping ||
			connection.registration.state !== RegistererState.Registered ||
			this.callSession
		)
			throw new Error("A registered idle connection is required.");
		const uri = UserAgent.makeURI(destination);
		if (!uri) throw new Error("Invalid call destination.");
		const session = new Inviter(connection.agent, uri, {
			sessionDescriptionHandlerOptions: {
				constraints: { audio: true, video: false },
			},
		});
		const owned = this.own(connection, session, capture, events);
		try {
			await session.invite();
		} catch (error: unknown) {
			await releaseFailedCall(
				error,
				(): Promise<void> => session.dispose(),
				(): void => {
					owned.release();
				},
			);
		}
	}

	public async answer(capture: MediaCapture): Promise<void> {
		const owned = this.callSession;
		if (!owned) throw new Error("An incoming call is required.");
		await owned.answer(capture);
	}

	public async hangup(): Promise<void> {
		await this.callSession?.hangup();
	}
}
