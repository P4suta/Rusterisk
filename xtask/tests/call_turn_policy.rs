use std::path::Path;
use xtask::inspect_call_turn;

#[test]
fn call_workers_reject_string_keys_and_lookups_by_the_name_on_a_call_event(
) -> Result<(), syn::Error> {
    let path = Path::new("crates/server/src/signaling.rs");
    for source in [
        "struct Signaling { calls: BTreeMap<String, Active> }",
        "struct Runner { answers: HashMap<ChannelId, Pending> }",
        "struct Signaling { pending: BTreeMap<CallKey, Pending> }",
        "fn turn(&mut self, key: CallKey) { self.channels.get_mut(&key.id); }",
    ] {
        assert!(!inspect_call_turn(path, source)?.is_empty(), "{source}");
    }
    for source in [
        "struct Signaling { records: CallRecords<Resources> }",
        "fn turn(&mut self, key: CallKey) { self.channels.by_handle(key.handle()); }",
        "struct Runner { subscribers: BTreeMap<String, Subscriber> }",
    ] {
        assert!(inspect_call_turn(path, source)?.is_empty(), "{source}");
    }
    assert!(inspect_call_turn(
        Path::new("crates/server/src/http.rs"),
        "fn request(&self, id: ChannelId) { self.channels.get(&id); }"
    )?
    .is_empty());
    Ok(())
}
