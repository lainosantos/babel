use super::*;
use serde_json::json;

fn fixture() -> Value {
    json!({
        "sinks": [
            {"index": 10, "name": "babel_mic_bus", "monitor_source": "babel_mic_bus.monitor"},
            {"index": 20, "name": "babel_speaker", "monitor_source": "babel_speaker.monitor"},
            {"index": 30, "name": "real_output", "monitor_source": "real_output.monitor"}
        ],
        "sources": [
            {"index": 11, "name": "babel_mic_bus.monitor", "monitor_source": "babel_mic_bus"},
            {"index": 12, "name": "babel_microphone", "owner_module": "102", "monitor_source": ""},
            {"index": 21, "name": "babel_speaker.monitor", "monitor_source": "babel_speaker"},
            {"index": 31, "name": "real_microphone", "monitor_source": ""}
        ],
        "modules": [
            {"index": 101, "name": "module-null-sink", "argument": "sink_name=babel_mic_bus sink_properties='babel.owner=org.babel.audio.v1'"},
            {"index": 102, "name": "module-remap-source", "argument": "source_name=babel_microphone master=babel_mic_bus.monitor source_properties='babel.owner=org.babel.audio.v1'"},
            {"index": 103, "name": "module-null-sink", "argument": "sink_name=babel_speaker sink_properties='babel.owner=org.babel.audio.v1'"}
        ],
        "source_outputs": [
            {"index": 200, "owner_module": "102", "source": 11, "corked": false, "properties": {"node.name": "input.babel_microphone"}},
            {"index": 201, "source": 31, "corked": false, "properties": {"babel.owner": "org.babel.audio.v1", "babel.target": "real_microphone"}},
            {"index": 202, "source": 21, "corked": false, "properties": {"application.id": "org.babel.audio", "babel.target": "babel_speaker.monitor"}}
        ],
        "sink_inputs": [
            {"index": 300, "sink": 10, "corked": false, "properties": {"babel.owner": "org.babel.audio.v1", "babel.target": "babel_mic_bus"}},
            {"index": 301, "sink": 30, "corked": false, "properties": {"application.id": "org.babel.audio", "babel.target": "real_output"}}
        ]
    })
}

fn external_clients(snapshot: &mut Value) {
    snapshot["source_outputs"].as_array_mut().unwrap().push(json!({
        "index": 210, "source": 12, "corked": false, "properties": {"application.name": "Calling app"}
    }));
    snapshot["sink_inputs"].as_array_mut().unwrap().push(json!({
        "index": 310, "sink": 20, "corked": false, "properties": {"application.name": "Calling app"}
    }));
}

fn evaluated(snapshot: &Value) -> UseSnapshot {
    evaluate(snapshot, "babel_mic_bus", "babel_speaker.monitor", None).unwrap()
}

#[test]
fn inactive_default_speaker_authorizes_tail_without_starting_capture() {
    let snapshot = fixture();
    let mut observed = evaluated(&snapshot);
    select_default_speaker(
        &mut observed,
        &snapshot,
        "babel_speaker.monitor",
        "babel_speaker",
    );
    assert!(observed.speaker_selected());
    assert!(observed.speaker_clients.is_empty());
    let (sender, receiver) = watch::channel(EndpointUse::default());
    publish_state(&sender, false, true, true, None, None, None);
    let epoch = receiver.borrow().speaker_selection_epoch;
    publish_state(
        &sender,
        false,
        false,
        observed.speaker_selected(),
        None,
        None,
        None,
    );
    assert!(!receiver.borrow().speaker);
    assert!(receiver.borrow().speaker_selected);
    assert_eq!(receiver.borrow().speaker_selection_epoch, epoch);
    select_default_speaker(
        &mut observed,
        &snapshot,
        "babel_speaker.monitor",
        "real_output",
    );
    publish_state(
        &sender,
        false,
        false,
        observed.speaker_selected(),
        None,
        None,
        None,
    );
    assert!(!receiver.borrow().speaker_selected);
    assert!(receiver.borrow().speaker_selection_epoch > epoch);
}

#[test]
fn paused_explicit_speaker_stream_keeps_selection_until_it_is_removed() {
    let mut snapshot = fixture();
    external_clients(&mut snapshot);
    snapshot["sink_inputs"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap()["corked"] = json!(true);
    let mut observed = evaluated(&snapshot);
    assert!(observed.speaker_selected());
    assert!(observed.speaker_clients.is_empty());
    let (sender, receiver) = watch::channel(EndpointUse::default());
    publish_state(&sender, false, false, true, None, None, None);
    apply_event(Event::RemovedSinkInput(310), &mut observed, &sender);
    assert!(!receiver.borrow().speaker_selected);
}

#[test]
fn internal_remap_and_babel_workers_are_not_external_clients() {
    let result = evaluated(&fixture());
    assert!(result.microphone_clients.is_empty());
    assert!(result.speaker_clients.is_empty());
    assert!(result.microphone_error.is_none());
    assert!(result.speaker_error.is_none());
}

#[test]
fn independent_external_clients_include_remap_and_direct_monitor() {
    let mut snapshot = fixture();
    external_clients(&mut snapshot);
    snapshot["source_outputs"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "index": 211, "source": 11, "corked": false
        }));
    let result = evaluated(&snapshot);
    assert_eq!(result.microphone_clients, HashSet::from([210, 211]));
    assert_eq!(result.speaker_clients, HashSet::from([310]));
    snapshot["sink_inputs"].as_array_mut().unwrap().pop();
    let result = evaluated(&snapshot);
    assert_eq!(result.microphone_clients.len(), 2);
    assert!(result.speaker_clients.is_empty());
}

#[test]
fn corked_clients_do_not_open_hardware_and_invalid_states_close_only_their_route() {
    let mut snapshot = fixture();
    external_clients(&mut snapshot);
    snapshot["source_outputs"][3]["corked"] = json!(true);
    snapshot["sink_inputs"][2]["corked"] = json!("yes");
    let result = evaluated(&snapshot);
    assert!(result.microphone_clients.is_empty());
    assert!(result.speaker_clients.is_empty());
    snapshot["source_outputs"][3]["corked"] = Value::Null;
    snapshot["sink_inputs"][2]["corked"] = json!(false);
    let result = evaluated(&snapshot);
    assert!(result.microphone_error.is_some());
    assert_eq!(result.speaker_clients, HashSet::from([310]));
}

#[test]
fn missing_endpoint_closes_only_the_affected_route() {
    let mut snapshot = fixture();
    external_clients(&mut snapshot);
    let result = evaluate(&snapshot, "missing_mic_bus", "babel_speaker.monitor", None).unwrap();
    assert!(result.microphone_clients.is_empty());
    assert!(
        result
            .microphone_error
            .as_ref()
            .unwrap()
            .contains("unavailable")
    );
    assert_eq!(result.speaker_clients, HashSet::from([310]));
    let result = evaluate(&snapshot, "babel_mic_bus", "missing_speaker.monitor", None).unwrap();
    assert_eq!(result.microphone_clients, HashSet::from([210]));
    assert!(result.speaker_clients.is_empty());
    assert!(
        result
            .speaker_error
            .as_ref()
            .unwrap()
            .contains("unavailable")
    );
}

#[test]
fn numeric_monitor_indices_and_remap_owner_module_are_supported() {
    let mut snapshot = fixture();
    external_clients(&mut snapshot);
    snapshot["sinks"][0]["monitor_source"] = json!(11);
    snapshot["sources"][0]["monitor_source"] = Value::Null;
    snapshot["sources"][0]["monitor_of_sink"] = json!(10);
    snapshot["sinks"][1]["monitor_source"] = json!(21);
    snapshot["sources"][2]["monitor_source"] = Value::Null;
    snapshot["sources"][2]["monitor_of_sink"] = json!(20);
    // The module ID also identifies the remap source if its public name changed.
    snapshot["sources"][1]["name"] = json!("renamed_microphone");
    let result = evaluated(&snapshot);
    assert_eq!(result.microphone_clients, HashSet::from([210]));
    assert_eq!(result.speaker_clients, HashSet::from([310]));
}

#[test]
fn moved_babel_stream_closes_its_direction_only() {
    for (collection, stream, field, wrong, microphone) in [
        ("source_outputs", 1, "source", 21, true),
        ("source_outputs", 2, "source", 31, false),
        ("sink_inputs", 0, "sink", 20, true),
        ("sink_inputs", 1, "sink", 10, false),
    ] {
        let mut snapshot = fixture();
        external_clients(&mut snapshot);
        snapshot[collection][stream][field] = json!(wrong);
        let result = evaluated(&snapshot);
        assert_eq!(result.microphone_error.is_some(), microphone);
        assert_eq!(result.speaker_error.is_some(), !microphone);
        assert_eq!(result.microphone_clients.is_empty(), microphone);
        assert_eq!(result.speaker_clients.is_empty(), !microphone);
    }
}

#[test]
fn unlinked_new_babel_stream_is_not_mistaken_for_a_moved_stream() {
    let mut snapshot = fixture();
    external_clients(&mut snapshot);
    snapshot["sink_inputs"][0]["sink"] = json!(u32::MAX);
    snapshot["source_outputs"][1]["source"] = Value::Null;
    let result = evaluated(&snapshot);
    assert!(result.microphone_error.is_none());
    assert!(result.speaker_error.is_none());
    assert_eq!(result.microphone_clients, HashSet::from([210]));
}

#[test]
fn ownership_is_exact_and_does_not_hide_unrelated_module_consumers() {
    let mut snapshot = fixture();
    snapshot["modules"][1]["argument"] = json!(
        "source_name=babel_microphone master=babel_mic_bus.monitor source_properties='babel.owner=org.babel.audio.v1.other'"
    );
    let result = evaluated(&snapshot);
    assert_eq!(result.microphone_clients, HashSet::from([200]));
}

#[test]
fn epochs_preserve_coalesced_transitions_and_do_not_restart_the_other_direction() {
    let (sender, mut receiver) = watch::channel(EndpointUse::default());
    publish(&sender, true, true, None, None, None);
    receiver.borrow_and_update();
    publish(&sender, false, true, None, None, None);
    publish(&sender, true, true, None, None, None);
    let value = receiver.borrow_and_update().clone();
    assert!(value.microphone && value.speaker);
    assert_eq!(value.microphone_epoch, 3);
    assert_eq!(value.speaker_epoch, 1);
    publish(&sender, true, true, None, None, None);
    assert!(!receiver.has_changed().unwrap());
    publish(
        &sender,
        false,
        true,
        Some("mic unavailable".into()),
        None,
        None,
    );
    assert_eq!(receiver.borrow().speaker_epoch, 1);
    publish(
        &sender,
        false,
        false,
        None,
        None,
        Some("server unavailable".into()),
    );
    assert_eq!(receiver.borrow().speaker_epoch, 2);
}

#[test]
fn remove_event_closes_last_client_and_preserves_the_other_route() {
    let (sender, receiver) = watch::channel(EndpointUse::default());
    publish(&sender, true, true, None, None, None);
    let mut snapshot = UseSnapshot {
        microphone_clients: HashSet::from([1, 2]),
        speaker_clients: HashSet::from([3]),
        ..UseSnapshot::default()
    };
    apply_event(Event::RemovedSourceOutput(1), &mut snapshot, &sender);
    assert!(receiver.borrow().microphone);
    apply_event(Event::RemovedSourceOutput(2), &mut snapshot, &sender);
    assert!(!receiver.borrow().microphone);
    assert!(receiver.borrow().speaker);
    assert_eq!(receiver.borrow().microphone_epoch, 2);
    assert_eq!(receiver.borrow().speaker_epoch, 1);
}

#[test]
fn short_modules_ignore_multiline_native_arguments_and_subscription_ignores_client_noise() {
    let modules = parse_short_modules(b"1\tlibpipewire-module-rt\t{\n nice.level = -11\n }\tn/a\n102\tmodule-remap-source\tsource_name=babel_microphone master=babel_mic_bus.monitor source_properties='babel.owner=org.babel.audio.v1'\tn/a\n").unwrap();
    assert_eq!(modules.len(), 2);
    assert_eq!(index(&modules[1]["index"]), Some(102));
    assert_eq!(argument(&modules[1], "babel.owner"), Some(OWNER));
    assert!(parse_event("Event 'new' on client #53").is_none());
    assert!(matches!(
        parse_event("Event 'remove' on source-output #12"),
        Some(Event::RemovedSourceOutput(12))
    ));
    assert!(matches!(
        parse_event("Event 'change' on sink-input #13"),
        Some(Event::Refresh)
    ));
}

#[tokio::test]
async fn oversized_snapshot_output_is_rejected_without_unbounded_allocation() {
    assert_eq!(read_limited(&b"1234"[..], 4).await.unwrap(), b"1234");
    assert!(read_limited(&b"12345"[..], 4).await.is_err());
}

#[test]
fn invalid_snapshot_is_global_inspection_failure() {
    assert!(evaluate(&json!({}), "babel_mic_bus", "babel_speaker.monitor", None).is_err());
}

#[test]
fn selected_system_mic_activates_without_clients_and_never_activates_output() {
    for name in ["babel_microphone", "babel_mic_bus.monitor"] {
        let snapshot = fixture();
        let result = evaluate(
            &snapshot,
            "babel_mic_bus",
            "babel_speaker.monitor",
            Some(name),
        )
        .unwrap();
        assert!(result.microphone_active(), "{name}");
        assert!(result.microphone_clients.is_empty());
        assert!(result.speaker_clients.is_empty());
    }
    for name in [
        "real_microphone",
        "babel_speaker.monitor",
        "babel_microphone_typo",
        "",
    ] {
        let result = evaluate(
            &fixture(),
            "babel_mic_bus",
            "babel_speaker.monitor",
            Some(name),
        )
        .unwrap();
        assert!(!result.microphone_active(), "{name}");
        assert!(result.speaker_clients.is_empty());
    }
}

#[test]
fn switching_system_mic_releases_route_but_explicit_app_capture_keeps_it_active() {
    let mut snapshot = fixture();
    let (sender, receiver) = watch::channel(EndpointUse::default());
    for (name, active, epoch) in [("babel_microphone", true, 1), ("real_microphone", false, 2)] {
        let result = evaluate(
            &snapshot,
            "babel_mic_bus",
            "babel_speaker.monitor",
            Some(name),
        )
        .unwrap();
        publish(&sender, result.microphone_active(), false, None, None, None);
        assert_eq!(receiver.borrow().microphone, active);
        assert_eq!(receiver.borrow().microphone_epoch, epoch);
        assert_eq!(receiver.borrow().speaker_epoch, 0);
    }
    external_clients(&mut snapshot);
    let result = evaluate(
        &snapshot,
        "babel_mic_bus",
        "babel_speaker.monitor",
        Some("real_microphone"),
    )
    .unwrap();
    assert!(result.microphone_active());
    assert!(!result.microphone_default);
}

#[test]
fn system_default_does_not_bypass_moved_stream_or_missing_endpoint_checks() {
    let mut snapshot = fixture();
    snapshot["source_outputs"][1]["source"] = json!(21);
    let result = evaluate(
        &snapshot,
        "babel_mic_bus",
        "babel_speaker.monitor",
        Some("babel_microphone"),
    )
    .unwrap();
    assert!(!result.microphone_active());
    assert!(result.microphone_error.is_some());
    let result = evaluate(
        &fixture(),
        "missing_mic",
        "babel_speaker.monitor",
        Some("babel_microphone"),
    )
    .unwrap();
    assert!(!result.microphone_active());
    assert!(result.microphone_error.is_some());
}

#[test]
fn removing_last_capture_client_does_not_close_system_selected_mic() {
    let mut snapshot = fixture();
    external_clients(&mut snapshot);
    let mut result = evaluate(
        &snapshot,
        "babel_mic_bus",
        "babel_speaker.monitor",
        Some("babel_microphone"),
    )
    .unwrap();
    let (sender, receiver) = watch::channel(EndpointUse::default());
    publish(&sender, true, true, None, None, None);
    apply_event(Event::RemovedSourceOutput(210), &mut result, &sender);
    assert!(receiver.borrow().microphone);
    assert_eq!(receiver.borrow().microphone_epoch, 1);
    assert!(result.microphone_clients.is_empty());
    apply_event(Event::RemovedSinkInput(310), &mut result, &sender);
    assert!(!receiver.borrow().speaker);
    assert!(receiver.borrow().microphone);
}
