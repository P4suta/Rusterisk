export interface Capture {
	stop(): void;
}

export interface ConnectionOptions {
	readonly server: string;
	readonly address: string;
	readonly username: string;
	readonly password: string;
	readonly iceServers: RTCIceServer[];
}

export interface SessionEvents {
	answered(): void;
	ended(): void;
}

export interface CallEvents {
	incoming(caller: string): SessionEvents | undefined;
	disconnected(): void;
}

export interface PhonePort<C extends Capture> {
	connect(options: ConnectionOptions, events: CallEvents): Promise<void>;
	disconnect(): Promise<void>;
	call(destination: string, capture: C, events: SessionEvents): Promise<void>;
	answer(capture: C): Promise<void>;
	hangup(): Promise<void>;
}

export interface PhoneState {
	readonly connection:
		| "disconnected"
		| "connecting"
		| "registered"
		| "disconnecting";
	readonly call:
		| "idle"
		| "call-microphone"
		| "answer-microphone"
		| "calling"
		| "incoming"
		| "answering"
		| "active"
		| "ending";
	readonly caller: string;
	readonly error: string;
}

export class Phone<C extends Capture> {
	private readonly port: PhonePort<C>;
	private readonly acquire: () => Promise<C>;
	private readonly changed: (state: PhoneState) => void;
	private capture: C | undefined;
	private connectionToken: number = 0;
	private callToken: number = 0;
	private captureToken: number = 0;
	private current: PhoneState = {
		connection: "disconnected",
		call: "idle",
		caller: "",
		error: "",
	};

	public constructor(
		port: PhonePort<C>,
		acquire: () => Promise<C>,
		changed: (state: PhoneState) => void,
	) {
		this.port = port;
		this.acquire = acquire;
		this.changed = changed;
	}

	public get state(): PhoneState {
		return this.current;
	}

	private publish(update: Partial<PhoneState>): void {
		this.current = Object.freeze({ ...this.current, ...update });
		this.changed(this.current);
	}

	private release(): void {
		this.callToken += 1;
		this.captureToken += 1;
		this.capture?.stop();
		this.capture = undefined;
	}

	private session(token: number): SessionEvents {
		return {
			answered: (): void => {
				if (
					this.callToken === token &&
					(this.current.call === "calling" || this.current.call === "answering")
				)
					this.publish({ call: "active" });
			},
			ended: (): void => {
				if (this.callToken !== token) return;
				this.release();
				this.publish({ call: "idle", caller: "" });
			},
		};
	}

	public async connect(options: ConnectionOptions): Promise<void> {
		if (this.current.connection !== "disconnected") return;
		const token = ++this.connectionToken;
		this.publish({ connection: "connecting", error: "" });
		try {
			await this.port.connect(options, {
				incoming: (caller: string): SessionEvents | undefined => {
					if (
						this.connectionToken !== token ||
						this.current.connection !== "registered" ||
						this.current.call !== "idle"
					)
						return undefined;
					this.release();
					this.publish({
						call: "incoming",
						caller: caller.slice(0, 256),
						error: "",
					});
					return this.session(this.callToken);
				},
				disconnected: (): void => {
					if (this.connectionToken !== token) return;
					this.connectionToken += 1;
					this.release();
					this.publish({
						connection: "disconnected",
						call: "idle",
						caller: "",
						error: "Connection lost. Connect again to receive calls.",
					});
				},
			});
			if (this.connectionToken === token)
				this.publish({ connection: "registered" });
		} catch {
			if (this.connectionToken === token)
				this.publish({
					connection: "disconnected",
					error:
						"Connection failed. Check the server address and account, then connect again.",
				});
		}
	}

	public async disconnect(): Promise<void> {
		if (
			this.current.connection === "disconnected" ||
			this.current.connection === "disconnecting"
		)
			return;
		this.connectionToken += 1;
		this.release();
		this.publish({
			connection: "disconnecting",
			call: "idle",
			caller: "",
			error: "",
		});
		try {
			await this.port.disconnect();
		} catch {
			this.publish({
				error: "Disconnect failed. Close the page to end the connection.",
			});
		}
		this.publish({ connection: "disconnected" });
	}

	private async start(destination: string | undefined): Promise<void> {
		const incoming = destination === undefined;
		if (
			this.current.connection !== "registered" ||
			this.current.call !== (incoming ? "incoming" : "idle")
		)
			return;
		const token = incoming ? this.callToken : ++this.callToken;
		const captureToken = ++this.captureToken;
		this.publish({
			call: incoming ? "answer-microphone" : "call-microphone",
			error: "",
		});
		try {
			const capture = await this.acquire();
			if (this.callToken !== token || this.captureToken !== captureToken) {
				capture.stop();
				return;
			}
			this.capture = capture;
			this.publish({ call: incoming ? "answering" : "calling" });
			if (destination === undefined) await this.port.answer(capture);
			else await this.port.call(destination, capture, this.session(token));
		} catch (error: unknown) {
			if (this.callToken !== token || this.captureToken !== captureToken)
				return;
			this.capture?.stop();
			this.capture = undefined;
			const denied =
				error instanceof DOMException && error.name === "NotAllowedError";
			this.publish({
				call: incoming ? "incoming" : "idle",
				error: denied
					? "Microphone permission was denied. Allow microphone access in browser settings, then try again."
					: "The call could not start. Check microphone availability and the destination, then try again.",
			});
		}
	}

	public async call(destination: string): Promise<void> {
		await this.start(destination);
	}
	public async answer(): Promise<void> {
		await this.start(undefined);
	}

	public async hangup(): Promise<void> {
		if (this.current.call === "idle" || this.current.call === "ending") return;
		if (this.current.call === "call-microphone") {
			this.release();
			this.publish({ call: "idle", caller: "", error: "" });
			return;
		}
		this.capture?.stop();
		this.capture = undefined;
		this.captureToken += 1;
		this.publish({ call: "ending", error: "" });
		try {
			await this.port.hangup();
		} catch {
			this.publish({
				error: "The call could not end. Disconnect to close the connection.",
			});
		}
	}
}
