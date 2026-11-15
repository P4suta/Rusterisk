#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let result = rusterisk_input_fuzz::controls(bytes);
    assert!(result.is_ok(), "{result:?}");
});
