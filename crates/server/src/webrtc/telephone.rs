use crate::media_link::DigitCommand;
use rusterisk_core::{DtmfToken, DtmfTone};
use rusterisk_media::{
    MediaError, RtpPacket, RtpSender, TelephoneEvent, TelephoneEvents, TelephoneFormat,
    TelephoneReceiver, TelephoneSender,
};

struct Reception {
    format: TelephoneFormat,
    receiver: TelephoneReceiver,
}

pub(super) struct TelephoneTransport {
    format: TelephoneFormat,
    sender: TelephoneSender,
    receivers: Vec<Reception>,
    active: Option<DigitCommand>,
}

pub(super) struct TelephoneOutput {
    pub packets: Vec<RtpPacket>,
    pub completed: Option<DtmfToken>,
}

impl TelephoneTransport {
    pub(super) fn new(
        audio: &RtpSender,
        source: u32,
        format: TelephoneFormat,
        formats: &[TelephoneFormat],
    ) -> Result<Self, MediaError> {
        if formats.len() > 16 || !formats.contains(&format) {
            return Err(MediaError::Unsupported);
        }
        let mut seed = [0; 10];
        getrandom::fill(&mut seed).map_err(super::negotiation_error)?;
        let generated = u32::from_be_bytes([seed[0], seed[1], seed[2], seed[3]]);
        if generated == 0 || generated == source {
            let next = source.wrapping_add(1).max(1);
            seed[..4].copy_from_slice(&next.to_be_bytes());
        }
        let receivers = formats
            .iter()
            .map(|format| {
                Ok(Reception {
                    format: *format,
                    receiver: TelephoneReceiver::new(format.payload_type(), format.rate())?,
                })
            })
            .collect::<Result<Vec<_>, MediaError>>()?;
        Ok(Self {
            format,
            sender: TelephoneSender::with_format(audio, format, seed)?,
            receivers,
            active: None,
        })
    }

    pub(super) fn receive(
        &mut self,
        packet: &RtpPacket,
        now: u64,
    ) -> Result<Option<TelephoneEvents>, MediaError> {
        let Some(reception) = self
            .receivers
            .iter_mut()
            .find(|reception| reception.format.payload_type() == packet.payload_type())
        else {
            return Ok(None);
        };
        let event = TelephoneEvent::parse(packet.payload())?;
        if !reception.format.supports(event.digit()) {
            return Err(MediaError::Unsupported);
        }
        reception.receiver.push(packet, now).map(Some)
    }

    pub(super) fn accepts(&self, payload: u8) -> bool {
        self.receivers
            .iter()
            .any(|receiver| receiver.format.payload_type() == payload)
    }

    pub(super) fn expire(&mut self, now: u64) -> Result<Vec<DtmfTone>, MediaError> {
        let mut tones = Vec::with_capacity(16);
        for reception in &mut self.receivers {
            if let Some(tone) = reception.receiver.expire(now)? {
                tones.push(tone);
            }
        }
        Ok(tones)
    }

    pub(super) fn discard_received(&mut self) {
        for reception in &mut self.receivers {
            reception.receiver.discard_pending();
        }
    }

    pub(super) fn idle(&self) -> bool {
        self.active.is_none()
    }

    pub(super) fn admit(&mut self, command: DigitCommand) -> Result<(), MediaError> {
        if self.active.is_some() {
            return Err(MediaError::Capacity);
        }
        self.active = Some(command);
        Ok(())
    }

    pub(super) fn cancel(&mut self) -> Option<DtmfToken> {
        self.sender.cancel();
        match self.active.take() {
            Some(DigitCommand::Sequence(command)) => Some(command.token),
            Some(DigitCommand::Tone(_)) | None => None,
        }
    }

    pub(super) fn timestamp_at(&self, now: u64, audio: &RtpSender) -> u32 {
        self.sender.timestamp_at(now, audio)
    }
    pub(super) fn rate(&self) -> u32 {
        self.format.rate()
    }

    pub(super) fn poll(
        &mut self,
        now: u64,
        audio: &mut RtpSender,
    ) -> Result<TelephoneOutput, MediaError> {
        let packets = self.sender.poll(now, audio)?;
        let mut completed = None;
        if let Some(command) = &mut self.active {
            match command {
                DigitCommand::Tone(tone) => {
                    if self.sender.ready() {
                        self.sender.start(*tone, now, audio)?;
                        self.active = None;
                    }
                }
                DigitCommand::Sequence(command) => {
                    let mut sequence = command.sequence;
                    let tone = sequence.advance(now).map_err(|error| {
                        MediaError::Transport(format!("Invalid digit schedule: {error:?}"))
                    })?;
                    if tone.is_none() || self.sender.ready() {
                        if let Some(tone) = tone {
                            self.sender.start(tone, now, audio)?;
                        }
                        command.sequence = sequence;
                        if sequence.finished() {
                            completed = Some(command.token);
                            self.active = None;
                        }
                    }
                }
            }
        }
        Ok(TelephoneOutput { packets, completed })
    }
}

#[cfg(test)]
mod tests {
    use super::TelephoneTransport;
    use crate::media_link::DigitCommand;
    use rusterisk_core::{DtmfDigit, DtmfOutbox, DtmfTiming, DtmfTone};
    use rusterisk_media::{
        AudioFormat, MediaError, RtpPacket, RtpSender, TelephoneEvent, TelephoneFormat,
    };

    fn transport() -> Result<TelephoneTransport, MediaError> {
        let audio = RtpSender::new(AudioFormat::Pcmu, 0, [0, 0, 0, 7, 0, 0, 0, 0, 0, 0])?;
        let format = TelephoneFormat::new(101, 8000)?;
        TelephoneTransport::new(&audio, 7, format, &[format])
    }

    fn packet(
        digit: DtmfDigit,
        timestamp: u32,
        duration: u16,
        ended: bool,
        sequence: u16,
    ) -> Result<RtpPacket, MediaError> {
        RtpPacket::new(
            101,
            sequence,
            timestamp,
            9,
            &TelephoneEvent::new(digit, ended, 10, duration)?.encode(),
        )
    }

    #[test]
    fn hold_preserves_completed_event_deduplication() -> Result<(), MediaError> {
        let mut transport = transport()?;
        let end = packet(DtmfDigit::Hash, 1000, 480, true, 1)?;
        let tones: Vec<_> = transport
            .receive(&end, 60)?
            .ok_or(MediaError::Unsupported)?
            .into_tones()
            .collect();
        assert_eq!(
            tones,
            [DtmfTone {
                digit: DtmfDigit::Hash,
                duration_ms: 60
            }]
        );
        transport.discard_received();
        assert_eq!(
            transport
                .receive(&packet(DtmfDigit::Hash, 1000, 480, true, 2)?, 80)?
                .ok_or(MediaError::Unsupported)?
                .into_tones()
                .count(),
            0
        );
        Ok(())
    }

    #[test]
    fn hold_discards_continuing_segments_until_a_distinct_event() -> Result<(), MediaError> {
        let mut transport = transport()?;
        transport.receive(&packet(DtmfDigit::One, 1000, 160, false, 1)?, 0)?;
        transport.discard_received();
        assert!(transport.expire(1000)?.is_empty());
        let segment = 1000 + u32::from(u16::MAX);
        assert_eq!(
            transport
                .receive(&packet(DtmfDigit::One, segment, 800, true, 2)?, 1100)?
                .ok_or(MediaError::Unsupported)?
                .into_tones()
                .count(),
            0
        );
        let tones: Vec<_> = transport
            .receive(
                &packet(DtmfDigit::One, segment + 1600, 480, true, 3)?.with_marker(true),
                1300,
            )?
            .ok_or(MediaError::Unsupported)?
            .into_tones()
            .collect();
        assert_eq!(
            tones,
            [DtmfTone {
                digit: DtmfDigit::One,
                duration_ms: 60
            }]
        );
        Ok(())
    }

    #[test]
    fn cancellation_releases_sequence_admission_once_and_clears_pending_packets(
    ) -> Result<(), MediaError> {
        let mut transport = transport()?;
        let mut audio = RtpSender::new(AudioFormat::Pcmu, 0, [0, 0, 0, 7, 0, 0, 0, 0, 0, 0])?;
        let mut outbox = DtmfOutbox::new(u16::MAX)
            .map_err(|error| MediaError::Transport(format!("{error:?}")))?;
        let timing = DtmfTiming::new(0, 0, 60, 100)
            .map_err(|error| MediaError::Transport(format!("{error:?}")))?;
        let command = outbox
            .admit(&[DtmfDigit::Hash, DtmfDigit::One], timing)
            .map_err(|error| MediaError::Transport(format!("{error:?}")))?;
        let token = command.token;
        transport.admit(DigitCommand::Sequence(command))?;
        assert!(transport.poll(0, &mut audio)?.packets.is_empty());
        assert_eq!(transport.poll(20, &mut audio)?.packets.len(), 1);
        assert_eq!(transport.cancel(), Some(token));
        assert!(outbox.finish(token));
        assert!(!outbox.finish(token));
        assert!(transport.cancel().is_none());
        assert!(transport.poll(1000, &mut audio)?.packets.is_empty());
        assert!(transport.idle());
        transport.admit(DigitCommand::Tone(DtmfTone {
            digit: DtmfDigit::Two,
            duration_ms: 20,
        }))?;
        transport.poll(1000, &mut audio)?;
        let packets = transport.poll(1020, &mut audio)?.packets;
        assert_eq!(packets.len(), 1);
        assert_eq!(
            TelephoneEvent::parse(packets[0].payload())?.digit(),
            DtmfDigit::Two
        );
        Ok(())
    }
}
