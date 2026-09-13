use caer_harness::{CommandEnvelope, EventEnvelope, HarnessCommand, HarnessEvent, PreworldAction};

#[test]
fn committed_command_fixture_is_accepted_and_round_trips() {
    let source = include_str!("fixtures/command_snapshot_v1.json");
    let command: CommandEnvelope = serde_json::from_str(source).unwrap();
    command.validate().unwrap();
    assert!(matches!(command.command, HarnessCommand::Snapshot));
    let encoded = serde_json::to_string(&command).unwrap();
    let decoded: CommandEnvelope = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, command);
}

#[test]
fn committed_event_fixture_is_accepted_and_heartbeat_is_not_progress() {
    let source = include_str!("fixtures/event_heartbeat_v1.json");
    let event: EventEnvelope = serde_json::from_str(source).unwrap();
    event.validate().unwrap();
    assert!(matches!(event.event, HarnessEvent::Heartbeat { .. }));
    assert!(!event.is_progress());
}

#[test]
fn unknown_major_fixture_is_red() {
    let source = include_str!("fixtures/command_snapshot_v1.json")
        .replace("caer.harness/v1", "caer.harness/v77");
    let command: CommandEnvelope = serde_json::from_str(&source).unwrap();
    assert!(command.validate().is_err());
}

#[test]
fn committed_typed_action_fixture_is_accepted() {
    let source = include_str!("fixtures/command_action_v1.json");
    let command: CommandEnvelope = serde_json::from_str(source).unwrap();
    command.validate().unwrap();
    assert!(matches!(
        command.command,
        HarnessCommand::ActivatePreworld {
            action: PreworldAction::CustomizeSlider { tick: 7, .. }
        }
    ));
}

#[test]
fn committed_hello_fixture_requires_complete_identity_axes() {
    let source = include_str!("fixtures/event_hello_v1.json");
    let event: EventEnvelope = serde_json::from_str(source).unwrap();
    event.validate().unwrap();
    assert!(matches!(event.event, HarnessEvent::Hello { .. }));
    let mut missing: serde_json::Value = serde_json::from_str(source).unwrap();
    missing["event"]["identity"]
        .as_object_mut()
        .unwrap()
        .remove("backend");
    assert!(serde_json::from_value::<EventEnvelope>(missing).is_err());
}
