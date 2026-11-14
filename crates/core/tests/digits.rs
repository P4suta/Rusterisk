use rusterisk_core::DtmfDigit;

#[test]
fn telephone_event_codes_roundtrip_only_the_sixteen_dtmf_symbols() {
    let symbols = "0123456789*#ABCD";
    for code in 0..=u8::MAX {
        let digit = DtmfDigit::from_event(code);
        assert_eq!(digit.is_some(), code < 16);
        if let Some(digit) = digit {
            assert_eq!(digit.event(), code);
            assert_eq!(Some(digit.symbol()), symbols.chars().nth(usize::from(code)));
            assert_eq!(DtmfDigit::from_symbol(digit.symbol()), Some(digit));
        }
    }
    for symbol in ['E', 'a', ' ', '\n', '０'] {
        assert!(DtmfDigit::from_symbol(symbol).is_none());
    }
}
