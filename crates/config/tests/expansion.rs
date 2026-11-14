use rusterisk_config::{expand_variables, ExpansionError};
use std::convert::Infallible;

#[test]
fn substitution_preserves_literals_and_reads_each_variable_without_recursive_evaluation(
) -> Result<(), ExpansionError<Infallible>> {
    let expanded = expand_variables(
        "${CALLERID(name)} <${CALLERID(num)}> ${EXTEN}/${CONTEXT}/${PRIORITY}: ${VALUE} ${MISSING}",
        |name| {
            Ok(match name {
                "CALLERID(name)" => "Alice",
                "CALLERID(num)" => "100",
                "EXTEN" => "200",
                "CONTEXT" => "internal",
                "PRIORITY" => "3",
                "VALUE" => "${CALLERID(num)}",
                "MISSING" => "",
                _ => "unexpected",
            }
            .to_owned())
        },
    )?;
    assert_eq!(expanded, "Alice <100> 200/internal/3: ${CALLERID(num)} ");
    assert_eq!(
        expand_variables("literal text", |_| Ok::<_, Infallible>(String::new()))?,
        "literal text"
    );
    Ok(())
}

#[test]
fn invalid_expressions_and_capacity_fail_before_returning_partial_values() {
    for input in [
        "${",
        "${}",
        "${A${B}}",
        "${SHELL(command)}",
        "${CALLERID(ani)}",
    ] {
        let mut resolved = 0;
        let result = expand_variables(input, |_| {
            resolved += 1;
            Ok::<_, Infallible>(String::new())
        });
        assert!(matches!(result, Err(ExpansionError::Syntax(_))), "{input}");
        assert_eq!(resolved, 0);
    }
    assert_eq!(
        expand_variables(&"x".repeat(4097), |_| Ok::<_, Infallible>(String::new())),
        Err(ExpansionError::Capacity)
    );
    assert_eq!(
        expand_variables("${X}${X}", |_| Ok::<_, Infallible>("x".repeat(2049))),
        Err(ExpansionError::Capacity)
    );
    assert_eq!(
        expand_variables("prefix ${X}", |_| Err::<String, _>(7u8)),
        Err(ExpansionError::Resolve(7))
    );
    assert_eq!(
        expand_variables("${X}", |_| Ok::<_, Infallible>("x".repeat(4096)))
            .map(|value| value.len()),
        Ok(4096)
    );
}
