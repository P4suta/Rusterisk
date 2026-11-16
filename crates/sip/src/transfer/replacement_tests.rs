use super::{Eligibility, Selection};
use crate::{ReferAuthority, ReferFailure, Replaces};

fn eligibility(index: u8) -> Eligibility {
    match index {
        0 => Eligibility::Confirmed,
        1 => Eligibility::Ended,
        _ => Eligibility::Unsupported,
    }
}

#[test]
fn multiple_matching_candidates_remain_ambiguous_regardless_of_eligibility() -> Result<(), String> {
    let config = rusterisk_config::Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[auth]\ntype=auth\nusername=phone\npassword=password\n[aor]\ntype=aor\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=aor\nallow=ulaw\n",
        "", "",
    ).map_err(|error| error.to_string())?;
    let endpoint = config.endpoints.first().ok_or("Missing endpoint")?;
    let authority = ReferAuthority::configured_peer(endpoint);
    let selector = Replaces::parse("consultation;to-tag=local;from-tag=remote")
        .map_err(|error| error.to_string())?;
    for first in 0..3 {
        for second in 0..3 {
            let mut selection = Selection::default();
            selection.include(&selector, "first", 1, &authority, eligibility(first));
            selection.include(&selector, "second", 2, &authority, eligibility(second));
            assert!(matches!(selection.finish(), Err(ReferFailure::NotFound)));
        }
    }
    let mut selection = Selection::default();
    for generation in 1..=3 {
        selection.include(
            &selector,
            "matched",
            generation,
            &authority,
            Eligibility::Confirmed,
        );
    }
    assert!(matches!(selection.finish(), Err(ReferFailure::NotFound)));
    Ok(())
}
