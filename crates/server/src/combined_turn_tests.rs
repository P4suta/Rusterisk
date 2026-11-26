// Measures one thread that owns the SIP engine and the control decisions.
// Each turn takes one input through the engine, the control decision, and the engine work that the decision asks for.
use crate::control::CallSettings;
use crate::{
    Action, CallKey, Control, ControlError, Effect, IncomingCall, IncomingTarget, Milestone, Signal,
};
use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    Method, Uri,
};
use rusterisk_api::ChannelId;
use rusterisk_config::Config;
use rusterisk_sip::{Engine, PreparedEngineConfiguration, SipEvent};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const CHANNELS: usize = 64;
const STAMP: &str = "2026-11-24T00:00:00.000+0000";

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

// One trusted calling endpoint and as many registering endpoints as the measurement has channels.
fn configuration(revision: usize) -> Result<Config, ControlError> {
    let mut objects = String::from(
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[trunkauth]\ntype=auth\nusername=trunk\npassword=password\n[trunk]\ntype=aor\n[trunk]\ntype=endpoint\ntransport=udp\nauth=trunkauth\naors=trunk\ncontext=internal\nallow=ulaw\n",
    );
    for index in 0..CHANNELS {
        objects.push_str(&format!(
            "[auth{index}]\ntype=auth\nusername=phone{index}\npassword=secret-{revision}\n[phone{index}]\ntype=aor\nmax_contacts=1\n[phone{index}]\ntype=endpoint\ntransport=udp\nauth=auth{index}\naors=phone{index}\ncontext=internal\nallow=ulaw\n"
        ));
    }
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        &objects,
        "",
        "[internal]\nexten => 100,1,Stasis(app)\n",
    )
    .map_err(failure)
}

fn peer(index: usize) -> Result<SocketAddr, ControlError> {
    let port = u16::try_from(20_000 + index).map_err(failure)?;
    Ok(SocketAddr::from(([127, 0, 0, 1], port)))
}

fn invite(index: usize, peer: SocketAddr, sequence: u32, authorization: &str) -> String {
    let body = format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=call\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0\r\na=sendrecv\r\n", 30_000 + index * 2);
    format!("INVITE sip:100@localhost SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch=z9hG4bK-invite-{index}-{sequence};rport\r\nFrom: <sip:caller{index}@localhost>;tag=caller-{index}\r\nTo: <sip:100@localhost>\r\nCall-ID: combined-{index}\r\nCSeq: {sequence} INVITE\r\nContact: <sip:caller{index}@{peer}>\r\nMax-Forwards: 70\r\n{authorization}Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", body.len())
}

// The digest answer of the calling endpoint to a challenge.
fn authorization(challenge: &[u8]) -> Result<String, ControlError> {
    let challenge = std::str::from_utf8(challenge)
        .map_err(failure)?
        .lines()
        .find_map(|line| line.strip_prefix("WWW-Authenticate: "))
        .ok_or(ControlError::Invalid("The engine sent no challenge header"))?;
    let challenge = WwwAuthenticate::parse(challenge).map_err(failure)?;
    let uri = Uri::try_from("sip:100@localhost").map_err(failure)?;
    let qop = AuthQop::Auth {
        cnonce: "combined".into(),
        nc: 1,
    };
    let digest = DigestGenerator {
        username: "trunk",
        password: "password",
        nonce: &challenge.nonce,
        uri: &uri,
        realm: &challenge.realm,
        method: &Method::Invite,
        qop: Some(&qop),
        algorithm: Algorithm::Md5,
    }
    .compute();
    Ok(format!("Authorization: Digest username=\"trunk\", realm=\"{}\", nonce=\"{}\", uri=\"sip:100@localhost\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"combined\", nc=00000001\r\n", challenge.realm, challenge.nonce))
}

// A request inside the dialog that the answer established.
fn in_dialog(
    answer: &[u8],
    method: &str,
    sequence: u32,
    peer: SocketAddr,
) -> Result<Vec<u8>, ControlError> {
    let answer = std::str::from_utf8(answer).map_err(failure)?;
    let header = |name: &str| {
        answer
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .ok_or(ControlError::Invalid("The answer lacks a dialog header"))
    };
    Ok(format!("{method} sip:100@127.0.0.1:5060 SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch=z9hG4bK-{method}-{sequence}-{};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} {method}\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", peer.port(), header("From: ")?, header("To: ")?, header("Call-ID: ")?).into_bytes())
}

struct Call {
    peer: SocketAddr,
    bye: Vec<u8>,
}

struct Combined {
    engine: Engine,
    control: Control,
    calls: Vec<Call>,
}

struct Turn {
    input: &'static str,
    elapsed: Duration,
}

impl Combined {
    // Answers every channel and bridges the channels in pairs, and records the turn of each input.
    fn established(turns: &mut Vec<Turn>) -> Result<Self, ControlError> {
        let config = configuration(1)?;
        let mut engine = Engine::new(config.clone(), [7; 32]);
        let mut control = Control::with_configuration(CHANNELS, &config);
        control.subscribe("app")?;
        let mut calls = Vec::with_capacity(CHANNELS);
        let mut keys = Vec::with_capacity(CHANNELS);
        for index in 0..CHANNELS {
            let peer = peer(index)?;
            let now = u64::try_from(index).map_err(failure)?;
            let initial = invite(index, peer, 1, "");
            let started = Instant::now();
            let challenged = engine
                .receive(initial.as_bytes(), peer, now)
                .map_err(failure)?;
            turns.push(Turn {
                input: "challenge",
                elapsed: started.elapsed(),
            });
            let challenge = challenged
                .datagrams
                .first()
                .ok_or(ControlError::Invalid("The engine sent no challenge"))?;
            let authorization = authorization(&challenge.bytes)?;
            let acknowledged = std::str::from_utf8(&in_dialog(&challenge.bytes, "ACK", 1, peer)?)
                .map_err(failure)?
                .replace(
                    &format!("z9hG4bK-ACK-1-{}", peer.port()),
                    &format!("z9hG4bK-invite-{index}-1"),
                );
            let started = Instant::now();
            engine
                .receive(acknowledged.as_bytes(), peer, now)
                .map_err(failure)?;
            turns.push(Turn {
                input: "challenge acknowledgment",
                elapsed: started.elapsed(),
            });
            let invitation = invite(index, peer, 2, &authorization);
            let started = Instant::now();
            let offered = engine
                .receive(invitation.as_bytes(), peer, now)
                .map_err(failure)?;
            let Some(SipEvent::Incoming {
                id,
                endpoint,
                caller,
                context,
                extension,
                ..
            }) = offered.events.into_iter().next()
            else {
                return Err(ControlError::Invalid("The engine refused an invitation"));
            };
            let id = ChannelId::try_from(id)?;
            control.incoming(
                IncomingCall {
                    id: id.clone(),
                    endpoint,
                    caller,
                    context,
                    extension,
                    target: IncomingTarget::Stasis {
                        application: "app".into(),
                        arguments: Vec::new(),
                    },
                },
                STAMP,
            )?;
            turns.push(Turn {
                input: "invitation",
                elapsed: started.elapsed(),
            });
            let key = control.key(&id)?;
            let local = SocketAddr::from((
                [127, 0, 0, 1],
                u16::try_from(40_000 + index * 2).map_err(failure)?,
            ));
            let started = Instant::now();
            control.apply(Action::Answer(id), STAMP, now)?;
            let answered = engine
                .answer(key.id.as_str(), key.generation, local, now)
                .map_err(failure)?;
            turns.push(Turn {
                input: "answer",
                elapsed: started.elapsed(),
            });
            let answer = answered
                .datagrams
                .first()
                .ok_or(ControlError::Invalid("The engine sent no answer"))?
                .bytes
                .clone();
            let acknowledged = in_dialog(&answer, "ACK", 2, peer)?;
            let started = Instant::now();
            engine.receive(&acknowledged, peer, now).map_err(failure)?;
            control.signal(
                Signal::Milestone {
                    key: key.clone(),
                    milestone: Milestone::Answered,
                },
                STAMP,
            )?;
            turns.push(Turn {
                input: "answer acknowledgment",
                elapsed: started.elapsed(),
            });
            calls.push(Call {
                peer,
                bye: in_dialog(&answer, "BYE", 3, peer)?,
            });
            keys.push(key);
        }
        for (index, pair) in keys.chunks(2).enumerate() {
            let bridge = ChannelId::try_from(format!("bridge-{index}"))?;
            let create = Action::CreateBridge {
                id: bridge.clone(),
                name: String::new(),
            };
            let started = Instant::now();
            control.apply(create, STAMP, 100)?;
            turns.push(Turn {
                input: "create bridge",
                elapsed: started.elapsed(),
            });
            let members = pair.iter().map(|key| key.id.clone()).collect();
            let started = Instant::now();
            control.apply(
                Action::AddChannels {
                    bridge,
                    channels: members,
                },
                STAMP,
                100,
            )?;
            turns.push(Turn {
                input: "bridge",
                elapsed: started.elapsed(),
            });
        }
        Ok(Self {
            engine,
            control,
            calls,
        })
    }

    // The peer hangs up: the engine ends the dialog, control ends the channel, and the engine releases it.
    fn hangup(&mut self, index: usize, now: u64) -> Result<Turn, ControlError> {
        let call = self.calls.get(index).ok_or(ControlError::Capacity)?;
        let started = Instant::now();
        let output = self
            .engine
            .receive(&call.bye, call.peer, now)
            .map_err(failure)?;
        let mut ended = 0;
        for event in output.events {
            if let SipEvent::Ended {
                id,
                generation,
                cause,
            } = event
            {
                let key = CallKey {
                    id: ChannelId::try_from(id)?,
                    generation,
                };
                let outcome = self
                    .control
                    .signal(Signal::Terminated { key, cause }, STAMP)?;
                for effect in outcome.effects {
                    if let Effect::Release { key } = effect {
                        self.engine
                            .cancel(key.id.as_str(), key.generation, now)
                            .map_err(failure)?;
                        ended += 1;
                    }
                }
            }
        }
        let elapsed = started.elapsed();
        if ended != 1 {
            return Err(ControlError::Invalid(
                "A hangup did not release its channel",
            ));
        }
        Ok(Turn {
            input: "hangup",
            elapsed,
        })
    }

    // One decision ends every call, and the engine releases each of them in the same turn.
    fn shutdown(&mut self, now: u64) -> Result<Turn, ControlError> {
        let started = Instant::now();
        let outcome = self.control.shutdown(STAMP)?;
        let mut released = 0;
        for effect in outcome.effects {
            if let Effect::Release { key } = effect {
                self.engine
                    .cancel(key.id.as_str(), key.generation, now)
                    .map_err(failure)?;
                released += 1;
            }
        }
        let elapsed = started.elapsed();
        if released != CHANNELS {
            return Err(ControlError::Invalid(
                "A shutdown did not release every channel",
            ));
        }
        Ok(Turn {
            input: "shutdown",
            elapsed,
        })
    }

    // The loader task parses and prepares the replacement, and the owner validates and commits it.
    fn reload(&mut self) -> Result<Turn, ControlError> {
        let replacement = configuration(2)?;
        let prepared =
            PreparedEngineConfiguration::prepare(replacement.clone()).map_err(failure)?;
        let settings = CallSettings::configured(&replacement);
        let started = Instant::now();
        let candidate = self.control.prepare_configuration(replacement, settings)?;
        self.control.commit_configuration(candidate)?;
        self.control.configuration_published(STAMP.into());
        self.engine.publish_configuration(prepared);
        Ok(Turn {
            input: "reload",
            elapsed: started.elapsed(),
        })
    }
}

// Every channel starts, answers, and joins a bridge.
// The first group reloads halfway through individual hangups.
// The second group reloads with all calls active and ends them in one shutdown decision.
fn trial() -> Result<Vec<Turn>, ControlError> {
    let mut turns = Vec::with_capacity(CHANNELS * 13 + 3);
    let mut combined = Combined::established(&mut turns)?;
    for index in 0..CHANNELS {
        if index == CHANNELS / 2 {
            turns.push(combined.reload()?);
        }
        turns.push(combined.hangup(index, 1000)?);
    }
    let mut ended = Combined::established(&mut turns)?;
    let starting_generation = ended.control.configuration_generation();
    turns.push(ended.reload()?);
    assert_ne!(
        ended.control.configuration_generation(),
        starting_generation
    );
    turns.push(ended.shutdown(2000)?);
    Ok(turns)
}

// The turn time that a share of the turns stays within.
fn percentile(mut values: Vec<Duration>, share: usize) -> Option<Duration> {
    values.sort_unstable();
    let index = (values.len() * share / 100).min(values.len().checked_sub(1)?);
    values.get(index).copied()
}

fn longest(turns: &[Turn]) -> Option<&Turn> {
    turns.iter().max_by_key(|turn| turn.elapsed)
}

fn median(mut values: Vec<Duration>) -> Option<Duration> {
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

#[test]
fn a_trial_times_each_input_and_reloads_before_the_simultaneous_shutdown(
) -> Result<(), ControlError> {
    let turns = trial()?;
    for (input, count) in [
        ("challenge", CHANNELS * 2),
        ("challenge acknowledgment", CHANNELS * 2),
        ("invitation", CHANNELS * 2),
        ("answer", CHANNELS * 2),
        ("answer acknowledgment", CHANNELS * 2),
        ("create bridge", CHANNELS),
        ("bridge", CHANNELS),
        ("hangup", CHANNELS),
        ("reload", 2),
        ("shutdown", 1),
    ] {
        assert_eq!(
            turns.iter().filter(|turn| turn.input == input).count(),
            count,
            "{input}"
        );
    }
    assert!(
        matches!(turns.as_slice(), [.., reload, shutdown] if reload.input == "reload" && shutdown.input == "shutdown")
    );
    Ok(())
}

// Run with `--ignored --nocapture` in a release build to read the measurement.
#[test]
#[ignore = "a measurement, which the decision record reads from a release build"]
fn measure_the_longest_combined_turn_in_five_trials() -> Result<(), ControlError> {
    let mut maxima = Vec::with_capacity(5);
    for number in 1..=5 {
        let turns = trial()?;
        let longest = longest(&turns).ok_or(ControlError::Capacity)?;
        let typical = median(turns.iter().map(|turn| turn.elapsed).collect())
            .ok_or(ControlError::Capacity)?;
        let tail = percentile(turns.iter().map(|turn| turn.elapsed).collect(), 99)
            .ok_or(ControlError::Capacity)?;
        let shutdown = turns
            .iter()
            .find(|turn| turn.input == "shutdown")
            .map(|turn| turn.elapsed)
            .ok_or(ControlError::Capacity)?;
        println!(
            "trial {number}: longest {:?} ({}), 99th percentile {tail:?}, median {typical:?}, shutdown {shutdown:?}, turns {}",
            longest.elapsed,
            longest.input,
            turns.len()
        );
        maxima.push(longest.elapsed);
    }
    let decided = median(maxima).ok_or(ControlError::Capacity)?;
    println!("median of the trial maxima: {decided:?}");
    Ok(())
}
