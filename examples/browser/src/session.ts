import { Invitation, type Inviter, SessionState, Web } from "sip.js";
import type { MediaCapture } from "./capture.ts";
import type { SessionEvents } from "./phone.ts";
import type { AudioOutput } from "./sip.ts";

export class OwnedSession {
	public readonly session: Invitation | Inviter;
	private readonly events: SessionEvents;
	private readonly output: AudioOutput;
	private readonly released: () => void;
	private readonly run: (operation: () => Promise<void>) => void;
	private capture: MediaCapture | undefined;
	private remote: MediaStream | undefined;
	private answered: boolean = false;
	private finished: boolean = false;
	private readonly changed: (state: SessionState) => void;

	public constructor(
		session: Invitation | Inviter,
		capture: MediaCapture | undefined,
		events: SessionEvents,
		output: AudioOutput,
		released: () => void,
		run: (operation: () => Promise<void>) => void,
	) {
		this.session = session;
		this.capture = capture;
		this.events = events;
		this.output = output;
		this.released = released;
		this.run = run;
		this.changed = (state: SessionState): void => {
			switch (state) {
				case SessionState.Initial:
				case SessionState.Establishing:
					break;
				case SessionState.Established: {
					if (this.answered) break;
					this.answered = true;
					this.events.answered();
					const handler = this.session.sessionDescriptionHandler;
					if (!(handler instanceof Web.SessionDescriptionHandler)) {
						this.run(
							(): Promise<void> =>
								Promise.reject(new Error("Call media is unavailable.")),
						);
						break;
					}
					this.remote = handler.remoteMediaStream;
					this.remote.addEventListener("addtrack", this.attach);
					this.attach();
					break;
				}
				case SessionState.Terminating:
					this.capture?.stop();
					break;
				case SessionState.Terminated:
					this.release();
					break;
			}
		};
		this.session.stateChange.addListener(this.changed);
	}

	private readonly attach: () => void = (): void => {
		const stream = this.remote;
		if (!this.finished && stream && stream.getAudioTracks().length > 0)
			this.run((): Promise<void> => this.output.attach(stream));
	};

	public description(
		iceServers: RTCIceServer[],
	): Web.SessionDescriptionHandler {
		return Web.defaultSessionDescriptionHandlerFactory(
			(): Promise<MediaStream> => {
				const capture = this.capture;
				if (
					this.finished ||
					!capture ||
					capture.stream.getAudioTracks().length !== 1 ||
					capture.stream.getVideoTracks().length > 0 ||
					capture.stream
						.getTracks()
						.some(
							(track: MediaStreamTrack): boolean => track.readyState !== "live",
						)
				)
					return Promise.reject(
						new Error("An owned audio capture is required."),
					);
				return Promise.resolve(capture.stream);
			},
		)(this.session, {
			iceGatheringTimeout: 4000,
			peerConnectionConfiguration: { iceServers },
		});
	}

	public async answer(capture: MediaCapture): Promise<void> {
		if (
			!(this.session instanceof Invitation) ||
			this.session.state !== SessionState.Initial ||
			this.finished
		)
			throw new Error("An incoming call is required.");
		this.capture = capture;
		await this.session.accept({
			sessionDescriptionHandlerOptions: {
				constraints: { audio: true, video: false },
			},
		});
	}

	public async hangup(): Promise<void> {
		switch (this.session.state) {
			case SessionState.Initial:
			case SessionState.Establishing:
				if (this.session instanceof Invitation) await this.session.reject();
				else await this.session.cancel();
				break;
			case SessionState.Established:
				await this.session.bye();
				break;
			case SessionState.Terminating:
			case SessionState.Terminated:
				break;
		}
	}

	public release(): void {
		if (this.finished) return;
		this.finished = true;
		this.session.stateChange.removeListener(this.changed);
		this.remote?.removeEventListener("addtrack", this.attach);
		this.capture?.stop();
		this.capture = undefined;
		this.released();
		this.events.ended();
	}
}
