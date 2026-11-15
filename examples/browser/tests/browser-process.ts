interface ProcessSource {
	readonly pid?: number | undefined;
	readonly exitCode: number | null;
	readonly signalCode: NodeJS.Signals | null;
	once(
		event: "exit" | "close",
		listener: (code: number | null, signal: NodeJS.Signals | null) => void,
	): void;
}

interface BrowserProcessServer {
	process(): ProcessSource;
	kill(): Promise<void>;
	wsEndpoint(): string;
}

interface ExitStatus {
	readonly code: number | null;
	readonly signal: NodeJS.Signals | null;
}

interface ExitEvent extends ExitStatus {
	readonly phase: "operation" | "cleanup";
}

export interface BrowserProcessState {
	readonly name: "Alice" | "Bob";
	readonly pid: number | null;
	readonly code: number | null;
	readonly signal: NodeJS.Signals | null;
	readonly beforeCleanup: ExitStatus | null;
	readonly exit: ExitEvent | null;
	readonly close: ExitEvent | null;
}

export class BrowserProcess {
	readonly #name: "Alice" | "Bob";
	readonly #server: BrowserProcessServer;
	readonly #process: ProcessSource;
	#beforeCleanup: ExitStatus | null = null;
	#exit: ExitEvent | null = null;
	#close: ExitEvent | null = null;

	constructor(name: "Alice" | "Bob", server: BrowserProcessServer) {
		this.#name = name;
		this.#server = server;
		this.#process = server.process();
		this.#process.once(
			"exit",
			(code: number | null, signal: NodeJS.Signals | null): void => {
				this.#exit = { code, signal, phase: this.#phase() };
			},
		);
		this.#process.once(
			"close",
			(code: number | null, signal: NodeJS.Signals | null): void => {
				this.#close = { code, signal, phase: this.#phase() };
			},
		);
	}

	wsEndpoint(): string {
		return this.#server.wsEndpoint();
	}

	kill(): Promise<void> {
		this.#beforeCleanup ??= {
			code: this.#process.exitCode,
			signal: this.#process.signalCode,
		};
		return this.#server.kill();
	}

	#phase(): "operation" | "cleanup" {
		return this.#beforeCleanup === null ? "operation" : "cleanup";
	}

	state(): BrowserProcessState {
		return {
			name: this.#name,
			pid: this.#process.pid ?? null,
			code: this.#process.exitCode,
			signal: this.#process.signalCode,
			beforeCleanup: this.#beforeCleanup,
			exit: this.#exit,
			close: this.#close,
		};
	}
}
