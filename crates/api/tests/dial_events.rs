use rusterisk_api::{Event, EventKind};

#[test]
fn dial_notifications_keep_optional_callers_and_forwarding_fields() -> Result<(), serde_json::Error>
{
    let channel: serde_json::Value = serde_json::from_str(
        r#"{"id":"external","name":"UnicastRTP/peer","state":"Up","accountcode":"","caller":{"name":"","number":""},"connected":{"name":"","number":""},"dialplan":{"context":"default","exten":"s","priority":1,"app_name":"Stasis","app_data":"calls"},"creationtime":"now","language":"en"}"#,
    )?;
    let mut value = serde_json::json!({"type":"Dial", "application":"calls", "timestamp":"now", "peer":channel, "dialstatus":"ANSWER"});
    let event: Event = serde_json::from_value(value.clone())?;
    assert!(matches!(event.kind, EventKind::Dial { .. }));
    assert_eq!(serde_json::to_value(event)?, value);
    value["caller"] = channel.clone();
    value["forwarded"] = channel;
    value["forward"] = "PJSIP/forwarded".into();
    value["dialstring"] = "PJSIP/phone".into();
    let event: Event = serde_json::from_value(value.clone())?;
    assert_eq!(serde_json::to_value(event)?, value);
    Ok(())
}
