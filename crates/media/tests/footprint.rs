use rusterisk_media::{AudioFormat, Frame, MediaError, RtpReceiver};

// A Tokio channel reserves a block of 32 slots on its first send, and a leg task held two frame channels.
const TASK_CHANNELS: usize = 2 * 32 * std::mem::size_of::<Frame>();
// A G.711 receiver held 4,568 bytes inline and in reserved memory when this budget took effect.
// The budget allows a quarter more, and a measure below 60 percent of it asks for a tighter budget.
const RECEIVER_BUDGET: usize = 4568 * 5 / 4;

#[test]
fn a_g711_receiver_stays_within_its_memory_budget() -> Result<(), MediaError> {
    let receiver = RtpReceiver::new(AudioFormat::Pcmu, 0)?;
    let held = std::mem::size_of::<RtpReceiver>() + receiver.reserved_bytes();
    const { assert!(TASK_CHANNELS > RECEIVER_BUDGET) };
    assert!(
        held <= RECEIVER_BUDGET,
        "a G.711 receiver holds {held} bytes"
    );
    assert!(
        held * 10 >= RECEIVER_BUDGET * 6,
        "a G.711 receiver holds {held} bytes, so the budget can shrink"
    );
    Ok(())
}
