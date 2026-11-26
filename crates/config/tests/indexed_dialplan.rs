use rusterisk_config::Dialplan;

#[test]
fn indexed_routes_preserve_declaration_order_specificity_and_hyphens_at_each_priority(
) -> Result<(), rusterisk_config::ConfigError> {
    let source = "[one]\nexten => _!,1,Hangup()\nexten => _X.,1,Hangup()\nexten => _[12]XX,1,Hangup()\nexten => _[21]XX,1,Hangup()\nexten => 1-23,1,Hangup()\nexten => 123,2,Hangup()\n[two]\nexten => 123,1,Hangup()\n";
    let program = Dialplan::parse(source)?;
    let indexed = program.clone().compile();
    for context in ["one", "two", "unknown"] {
        for extension in ["1-23", "123", "124", "222", "345", "", "7", "2-2-2"] {
            for priority in [1, 2, 3] {
                assert_eq!(
                    indexed.position(context, extension, priority),
                    program.position(context, extension, priority),
                    "{context}/{extension}/{priority}"
                );
            }
        }
    }
    assert_eq!(indexed.position("one", "124", 1), Some(2));
    assert_eq!(indexed.position("one", "123", 1), Some(4));
    Ok(())
}
