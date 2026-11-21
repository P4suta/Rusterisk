use super::{credentials, error, id, parameters};
use crate::{
    media::{MediaPort, ProbeProgress},
    Action, ControlError, Controller, Credentials, Subscription,
};
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header, HeaderMap, Uri},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use futures_util::StreamExt;
use rusterisk_api::{
    test_call::{TestCallOutcome as Outcome, TestCallResult, DEFAULT_SECONDS, MAX_SECONDS, PATH},
    ChannelId, ChannelState, EventKind,
};
use std::{collections::BTreeMap, time::Duration};
use tokio::time::{timeout_at, Instant};

pub(super) fn router(controller: Controller, media: Option<MediaPort>) -> Router {
    Router::new()
        .route(PATH, post(start))
        .with_state((controller, media))
}

struct Probe {
    controller: Controller,
    media: MediaPort,
    credentials: Credentials,
    origin: Option<String>,
    subscription: Subscription,
    channel: ChannelId,
    endpoint: String,
    seconds: u16,
    deadline: Instant,
}

impl Probe {
    async fn run(mut self) -> TestCallResult {
        let mut result = TestCallResult {
            channel_id: self.channel.clone(),
            accepted: false,
            answered: false,
            signal_verified: false,
            outcome: Outcome::NotAccepted,
        };
        let operation = Action::Originate {
            id: self.channel.clone(),
            endpoint: self.endpoint.clone(),
            app: self.subscription.application.clone(),
            args: Vec::new(),
            caller: String::new(),
            timeout: self.seconds.into(),
        };
        if !matches!(
            timeout_at(
                self.deadline,
                self.controller
                    .execute(self.credentials.clone(), self.origin.clone(), operation)
            )
            .await,
            Ok(Ok(_))
        ) {
            return result;
        }
        result.accepted = true;
        loop {
            let Ok(Some(event)) = timeout_at(self.deadline, self.subscription.events.recv()).await
            else {
                result.outcome = Outcome::NoAnswer;
                return result;
            };
            if matches!(&event.kind,EventKind::StasisStart{channel,..} if channel.id==self.channel && channel.state==ChannelState::Up)
            {
                break;
            }
            if let EventKind::ChannelDestroyed { channel, cause, .. } = event.kind {
                if channel.id == self.channel {
                    result.outcome = match cause {
                        17 => Outcome::Busy,
                        18 | 19 => Outcome::NoAnswer,
                        _ => Outcome::CallEnded,
                    };
                    return result;
                }
            }
        }
        result.answered = true;
        let key = timeout_at(
            self.deadline,
            self.controller.observed_channel(
                self.credentials.clone(),
                self.origin.clone(),
                self.channel.clone(),
            ),
        )
        .await;
        let Ok(Ok((key, _))) = key else {
            result.outcome = Outcome::CallEnded;
            return result;
        };
        let Ok(mut audio) = self.media.test_signal(key) else {
            result.outcome = Outcome::MediaUnavailable;
            return result;
        };
        result.outcome = loop {
            if *audio.borrow_and_update() == ProbeProgress::Verified {
                break Outcome::Verified;
            }
            tokio::select! {
                changed=audio.changed()=>if changed.is_err(){break Outcome::CallEnded;},
                event=self.subscription.events.recv()=>match event{
                    Some(event) if matches!(event.kind,EventKind::ChannelDestroyed{ref channel,..} if channel.id==self.channel)=>break Outcome::CallEnded,
                    None=>break Outcome::CallEnded,Some(_)=>{}
                },
                _deadline=tokio::time::sleep_until(self.deadline)=>break match *audio.borrow(){ProbeProgress::Silent=>Outcome::Silent,ProbeProgress::OtherAudio=>Outcome::SignalMismatch,ProbeProgress::Verified=>Outcome::Verified},
            }
        };
        result.signal_verified = result.outcome == Outcome::Verified;
        result
    }
}

async fn start(
    State((controller, media)): State<(Controller, Option<MediaPort>)>,
    Path((technology, resource)): Path<(String, String)>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let prepare = async {
        let auth = credentials(&headers, &BTreeMap::new())?;
        let origin = headers
            .get(header::ORIGIN)
            .map(|h| h.to_str().map(str::to_owned))
            .transpose()
            .map_err(|_| ControlError::Forbidden)?;
        controller
            .authorize(auth.clone(), origin.clone(), false)
            .await?;
        if technology != "PJSIP" || !rusterisk_api::is_valid_resource_id(&resource) {
            return Err(ControlError::Invalid(
                "Select a configured PJSIP echo endpoint",
            ));
        }
        let mut query = parameters(&uri)?;
        let seconds = query
            .remove("deadline_seconds")
            .map(|value| value.parse::<u16>())
            .transpose()
            .map_err(|_| ControlError::Invalid("Invalid test call deadline"))?
            .unwrap_or(DEFAULT_SECONDS);
        if !query.is_empty() || !(1..=MAX_SECONDS).contains(&seconds) {
            return Err(ControlError::Invalid("Invalid test call options"));
        }
        let deadline = Instant::now() + Duration::from_secs(u64::from(seconds));
        let media = media.ok_or(ControlError::Unsupported)?;
        controller
            .execute(
                auth.clone(),
                origin.clone(),
                Action::GetEndpoint {
                    technology,
                    resource: resource.clone(),
                },
            )
            .await?;
        let suffix = uuid::Uuid::new_v4().simple();
        let name = format!("probe-app-{suffix}");
        let channel = id(&format!("probe-{suffix}"))?;
        let subscription = controller
            .subscribe(auth.clone(), origin.clone(), name)
            .await?;
        Ok(Probe {
            controller,
            media,
            credentials: auth,
            origin,
            subscription,
            channel,
            endpoint: format!("PJSIP/{resource}"),
            seconds,
            deadline,
        })
    }
    .await;
    let probe = match prepare {
        Ok(probe) => probe,
        Err(problem) => return error(problem),
    };
    let initial = futures_util::stream::iter(std::iter::once(Ok::<Bytes, serde_json::Error>(
        Bytes::from_static(b" "),
    )));
    let result = futures_util::stream::once(async move {
        serde_json::to_vec(&probe.run().await).map(Bytes::from)
    });
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(initial.chain(result)),
    )
        .into_response()
}
