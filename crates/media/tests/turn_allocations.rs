use rusterisk_media::{AudioFormat, Frame, MediaError, Mixer, RtpPacket, RtpReceiver, RtpSender};

// Two G.711 legs in one bridge: each peer sends a packet, the mixer joins them, and each leg sends the mix.
struct Path {
    peers: [RtpSender; 2],
    receivers: [RtpReceiver; 2],
    senders: [RtpSender; 2],
    mixer: Mixer<u32>,
    heard: usize,
}

impl Path {
    fn new() -> Result<Self, MediaError> {
        let mut mixer = Mixer::new(4);
        for key in [0, 1] {
            mixer.insert(key)?;
        }
        mixer.bridge(1, &[0, 1])?;
        Ok(Self {
            peers: [
                RtpSender::new(AudioFormat::Pcmu, 0, [1; 10])?,
                RtpSender::new(AudioFormat::Pcmu, 0, [2; 10])?,
            ],
            receivers: [
                RtpReceiver::new(AudioFormat::Pcmu, 0)?,
                RtpReceiver::new(AudioFormat::Pcmu, 0)?,
            ],
            senders: [
                RtpSender::new(AudioFormat::Pcmu, 0, [3; 10])?,
                RtpSender::new(AudioFormat::Pcmu, 0, [4; 10])?,
            ],
            mixer,
            heard: 0,
        })
    }

    // One 20 millisecond turn of both legs.
    fn turn(&mut self, now: u64) -> Result<(), MediaError> {
        for (index, sample) in [(0, 4000), (1, -7000)] {
            let spoken = Frame::new(&[sample; 160], 8000)?;
            let wire = self.peers[index].packet_at(spoken, now)?.encode()?;
            let received = RtpPacket::parse(&wire)?;
            self.receivers[index].push(&received)?;
            if let Some(frame) = self.receivers[index].pop()? {
                self.mixer.push(&(index as u32), frame)?;
            }
        }
        let mixed = self.mixer.tick_with_music(&[], &[])?;
        for (key, frame) in &mixed.channels {
            let index = *key as usize;
            let sent = self.senders[index].packet_at(*frame, now)?.encode()?;
            self.heard += sent.len();
        }
        Ok(())
    }
}

// After one warm turn, the per-frame media path takes no heap memory.
#[test]
fn the_per_frame_media_path_allocates_nothing_after_one_warm_turn() -> Result<(), MediaError> {
    let mut path = Path::new()?;
    path.turn(0)?;
    let mut result = Ok(());
    let counted = allocation_counter::measure(|| {
        for turn in 1..50 {
            if let Err(error) = path.turn(turn * 20) {
                result = Err(error);
                return;
            }
        }
    });
    result?;
    assert!(path.heard > 0);
    assert_eq!(counted.count_total, 0, "{counted:?}");
    Ok(())
}
