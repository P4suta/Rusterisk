use xtask::native_sanitizers::{validate_rejection, Defect, NativeInventory, Scope};

const LIST: &str = "opus_transmits_a_non_silent_known_signal: test\n1 test, 0 benchmarks\nopus_packets_cover_each_frame_duration_and_the_full_120_millisecond_packet: test\n1 test, 0 benchmarks\nopus_packet_durations_preserve_decoded_samples_in_complete_mixer_frames: test\n1 test, 0 benchmarks\n";
const RUN: &str = "running 1 test\ntest opus_transmits_a_non_silent_known_signal ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\nrunning 1 test\ntest opus_packets_cover_each_frame_duration_and_the_full_120_millisecond_packet ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\nrunning 1 test\ntest opus_packet_durations_preserve_decoded_samples_in_complete_mixer_frames ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";

#[test]
fn native_boundary_verdict_requires_every_discovered_test_and_terminal_summary(
) -> Result<(), std::io::Error> {
    let inventory = NativeInventory::parse(Scope::Media, LIST.as_bytes())?;
    assert_eq!(inventory.validate(RUN.as_bytes())?, 3);
    for output in [
        String::new(),
        RUN.replace("3 passed", "0 passed").replace("1 passed", "0 passed"),
        RUN.replacen("0 ignored", "1 ignored", 1),
        RUN.replacen("0 failed", "1 failed", 1),
        RUN.replacen("... ok", "... FAILED", 1),
        RUN.replacen("opus_transmits_a_non_silent_known_signal", "unknown", 1),
        format!("{RUN}test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n"),
        format!("{RUN}runtime error: signed integer overflow\n"),
        format!("{RUN}ERROR: AddressSanitizer: heap-use-after-free\n"),
    ] {
        assert!(inventory.validate(output.as_bytes()).is_err(), "{output}");
    }
    let truncated = RUN
        .rsplit_once("test result:")
        .ok_or_else(|| std::io::Error::other("fixture summary missing"))?
        .0;
    assert!(inventory.validate(truncated.as_bytes()).is_err());
    Ok(())
}

#[test]
fn native_inventory_rejects_missing_scopes_duplicates_and_empty_harnesses() {
    for listing in [
        String::new(),
        LIST.replace("opus_transmits_a_non_silent_known_signal: test\n", ""),
        format!("{LIST}opus_transmits_a_non_silent_known_signal: test\n"),
        LIST.replace("1 test, 0 benchmarks", "0 tests, 0 benchmarks"),
    ] {
        assert!(NativeInventory::parse(Scope::Media, listing.as_bytes()).is_err());
    }
}

#[test]
fn native_defect_probes_require_the_expected_instrumented_diagnostic_and_exit() {
    let address = b"ERROR: AddressSanitizer: heap-use-after-free\n";
    let undefined = b"runtime error: signed integer overflow\n";
    assert!(validate_rejection(Defect::Address, Some(86), address).is_ok());
    assert!(validate_rejection(Defect::Undefined, Some(87), undefined).is_ok());
    for code in [None, Some(0), Some(1), Some(124), Some(125), Some(137)] {
        assert!(validate_rejection(Defect::Address, code, address).is_err());
        assert!(validate_rejection(Defect::Undefined, code, undefined).is_err());
    }
    assert!(validate_rejection(Defect::Address, Some(86), undefined).is_err());
    assert!(validate_rejection(Defect::Undefined, Some(87), address).is_err());
}
