use super::*;

fn process(pid: u32, inputs: &[u32], outputs: &[u32]) -> ProcessUse {
    ProcessUse {
        object_id: pid + 1000,
        pid,
        running_input: !inputs.is_empty(),
        running_output: !outputs.is_empty(),
        input_devices: inputs.to_vec(),
        output_devices: outputs.to_vec(),
    }
}

#[test]
fn physical_microphone_and_virtual_speaker_in_one_app_only_activate_speaker() {
    let usage = classify_processes(42, Some(10), Some(20), &[process(100, &[30], &[20])]);
    assert!(usage.microphone_clients.is_empty());
    assert_eq!(usage.speaker_clients, BTreeSet::from([(1100, 100)]));
}

#[test]
fn virtual_microphone_and_physical_output_in_one_app_only_activate_microphone() {
    let usage = classify_processes(42, Some(10), Some(20), &[process(100, &[10], &[40])]);
    assert_eq!(usage.microphone_clients, BTreeSet::from([(1100, 100)]));
    assert!(usage.speaker_clients.is_empty());
}

#[test]
fn babel_and_inactive_processes_never_keep_a_virtual_cable_active() {
    let own = process(42, &[10], &[20]);
    let mut inactive = process(100, &[10], &[20]);
    inactive.running_input = false;
    inactive.running_output = false;
    let usage = classify_processes(42, Some(10), Some(20), &[own, inactive]);
    assert!(usage.microphone_clients.is_empty());
    assert!(usage.speaker_clients.is_empty());
}

#[test]
fn device_in_opposite_scope_does_not_activate_the_route() {
    let usage = classify_processes(42, Some(10), Some(20), &[process(100, &[20], &[10])]);
    assert!(usage.microphone_clients.is_empty());
    assert!(usage.speaker_clients.is_empty());
}

#[test]
fn aliases_resolving_to_the_same_device_never_open_feedback_routes() {
    let usage = classify_processes(42, Some(10), Some(10), &[process(100, &[10], &[10])]);
    assert!(usage.microphone_clients.is_empty());
    assert!(usage.speaker_clients.is_empty());
    assert!(usage.error.is_some());
}

#[test]
fn input_inspection_error_closes_only_microphone_and_preserves_speaker() {
    let (state, receiver) = tokio::sync::watch::channel(EndpointUse::default());
    let mut tracker = UseTracker::default();
    let active = classify_processes(42, Some(10), Some(20), &[process(100, &[10], &[20])]);
    tracker.publish(&state, active.clone());
    let prior = receiver.borrow().clone();
    assert!(prior.microphone && prior.speaker);
    tracker.publish(
        &state,
        UseSnapshot {
            microphone_error: Some("input query failed".into()),
            ..active
        },
    );
    let current = receiver.borrow();
    assert!(!current.microphone && current.speaker);
    assert!(current.microphone_epoch > prior.microphone_epoch);
    assert_eq!(current.speaker_epoch, prior.speaker_epoch);
}

#[test]
fn process_api_unavailable_closes_both_directions() {
    let (state, receiver) = tokio::sync::watch::channel(EndpointUse::default());
    let mut tracker = UseTracker::default();
    let active = classify_processes(42, Some(10), Some(20), &[process(100, &[10], &[20])]);
    tracker.publish(&state, active.clone());
    tracker.publish(
        &state,
        UseSnapshot {
            error: Some("process objects unsupported".into()),
            ..active
        },
    );
    assert!(!receiver.borrow().microphone);
    assert!(!receiver.borrow().speaker);
    assert!(receiver.borrow().error.is_some());
}

#[test]
fn replacement_client_invalidates_old_queue_even_when_watch_coalesces_activity() {
    let (state, receiver) = tokio::sync::watch::channel(EndpointUse::default());
    let mut tracker = UseTracker::default();
    let initial = classify_processes(42, Some(10), Some(20), &[process(100, &[], &[20])]);
    tracker.publish(&state, initial.clone());
    let first = receiver.borrow().clone();
    tracker.publish(&state, initial);
    assert_eq!(receiver.borrow().speaker_epoch, first.speaker_epoch);
    let replacement = classify_processes(42, Some(10), Some(20), &[process(200, &[], &[20])]);
    tracker.publish(&state, replacement);
    assert!(receiver.borrow().speaker);
    assert!(receiver.borrow().speaker_epoch > first.speaker_epoch);
    assert_eq!(receiver.borrow().microphone_epoch, first.microphone_epoch);
}

#[test]
fn endpoint_reconnection_with_new_object_id_invalidates_only_that_direction() {
    let (state, receiver) = tokio::sync::watch::channel(EndpointUse::default());
    let mut tracker = UseTracker::default();
    tracker.publish(
        &state,
        classify_processes(42, Some(10), Some(20), &[process(100, &[10], &[20])]),
    );
    let prior = receiver.borrow().clone();
    tracker.publish(
        &state,
        classify_processes(42, Some(11), Some(20), &[process(100, &[11], &[20])]),
    );
    assert!(receiver.borrow().microphone && receiver.borrow().speaker);
    assert!(receiver.borrow().microphone_epoch > prior.microphone_epoch);
    assert_eq!(receiver.borrow().speaker_epoch, prior.speaker_epoch);
}

fn observation() -> Observation {
    Observation {
        started: Instant::now(),
        microphone_setting: "mic".into(),
        speaker_setting: "speaker".into(),
        snapshot: classify_processes(42, Some(10), Some(20), &[process(100, &[10], &[20])]),
    }
}

#[test]
fn slow_hal_query_and_queued_stale_snapshot_cannot_activate_audio() {
    let mut late = observation();
    late.started = Instant::now() - SNAPSHOT_TIMEOUT;
    let snapshot = late.validate(Instant::now(), "mic", "speaker");
    assert!(snapshot.microphone_clients.is_empty());
    assert!(snapshot.speaker_clients.is_empty());
    assert!(snapshot.error.is_some());
}

#[test]
fn snapshot_for_previous_selection_does_not_reactivate_that_direction() {
    let snapshot = observation().validate(Instant::now(), "replacement-mic", "speaker");
    assert!(snapshot.microphone_clients.is_empty());
    assert!(!snapshot.speaker_clients.is_empty());
    assert!(snapshot.microphone_device.is_none());
}

#[tokio::test(start_paused = true)]
async fn watchdog_closes_stalled_queries_and_fresh_observation_can_recover() {
    let (_mic, mic_watch) = tokio::sync::watch::channel("mic".to_owned());
    let (_speaker, speaker_watch) = tokio::sync::watch::channel("speaker".to_owned());
    let (state, mut receiver) = tokio::sync::watch::channel(EndpointUse::default());
    let (updates, observations) = tokio::sync::mpsc::channel(1);
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(supervise(
        mic_watch,
        speaker_watch,
        cancel.clone(),
        state,
        observations,
    ));
    updates.send(observation()).await.unwrap();
    receiver.changed().await.unwrap();
    let initial = receiver.borrow_and_update().clone();
    assert!(initial.microphone && initial.speaker);
    tokio::time::advance(SNAPSHOT_TIMEOUT).await;
    receiver.changed().await.unwrap();
    let timed_out = receiver.borrow_and_update().clone();
    assert!(!timed_out.microphone && !timed_out.speaker);
    assert!(timed_out.error.as_deref().unwrap().contains("timed out"));
    assert!(timed_out.microphone_epoch > initial.microphone_epoch);
    assert!(timed_out.speaker_epoch > initial.speaker_epoch);
    updates.send(observation()).await.unwrap();
    receiver.changed().await.unwrap();
    assert!(receiver.borrow().microphone && receiver.borrow().speaker);
    assert!(receiver.borrow().error.is_none());
    cancel.cancel();
    task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn changed_selection_closes_immediately_while_hal_worker_is_busy() {
    let (mic, mic_watch) = tokio::sync::watch::channel("mic".to_owned());
    let (_speaker, speaker_watch) = tokio::sync::watch::channel("speaker".to_owned());
    let (state, mut receiver) = tokio::sync::watch::channel(EndpointUse::default());
    let (updates, observations) = tokio::sync::mpsc::channel(1);
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn(supervise(
        mic_watch,
        speaker_watch,
        cancel.clone(),
        state,
        observations,
    ));
    updates.send(observation()).await.unwrap();
    receiver.changed().await.unwrap();
    receiver.borrow_and_update();
    mic.send_replace("replacement-mic".into());
    receiver.changed().await.unwrap();
    assert!(!receiver.borrow().microphone);
    assert!(receiver.borrow().speaker);
    cancel.cancel();
    task.await.unwrap();
}

#[test]
fn system_input_selection_activates_only_mic_and_releases_on_physical_selection() {
    let (state, receiver) = tokio::sync::watch::channel(EndpointUse::default());
    let mut tracker = UseTracker::default();
    let mut snapshot = classify_processes(42, Some(10), Some(20), &[]);
    snapshot.select_default_microphone(Some(10));
    tracker.publish(&state, snapshot.clone());
    let epoch = receiver.borrow().microphone_epoch;
    assert!(receiver.borrow().microphone);
    assert!(!receiver.borrow().speaker);
    for default in [Some(30), Some(20), Some(0), None] {
        snapshot.select_default_microphone(default);
        tracker.publish(&state, snapshot.clone());
        assert!(!receiver.borrow().microphone);
        assert!(!receiver.borrow().speaker);
    }
    assert!(receiver.borrow().microphone_epoch > epoch);
    snapshot = classify_processes(42, Some(10), Some(20), &[process(100, &[10], &[])]);
    snapshot.select_default_microphone(Some(30));
    tracker.publish(&state, snapshot);
    assert!(receiver.borrow().microphone);
}

#[test]
fn selected_system_input_does_not_survive_errors_or_stale_selection() {
    let (state, receiver) = tokio::sync::watch::channel(EndpointUse::default());
    let mut tracker = UseTracker::default();
    let mut observed = observation();
    observed.snapshot.select_default_microphone(Some(10));
    let snapshot = observed.validate(Instant::now(), "replacement-mic", "speaker");
    assert!(!snapshot.microphone_default);
    tracker.publish(&state, snapshot);
    assert!(!receiver.borrow().microphone);
    let mut snapshot = classify_processes(42, Some(10), Some(20), &[]);
    snapshot.select_default_microphone(Some(10));
    snapshot.microphone_error = Some("device disconnected".into());
    tracker.publish(&state, snapshot);
    assert!(!receiver.borrow().microphone);
    let mut snapshot = classify_processes(42, Some(10), Some(10), &[]);
    snapshot.select_default_microphone(Some(10));
    tracker.publish(&state, snapshot);
    assert!(!receiver.borrow().microphone);
    assert!(!receiver.borrow().speaker);
}

#[test]
fn default_mic_remains_active_after_last_external_client_closes() {
    let (state, receiver) = tokio::sync::watch::channel(EndpointUse::default());
    let mut tracker = UseTracker::default();
    let mut snapshot = classify_processes(42, Some(10), Some(20), &[process(100, &[10], &[])]);
    snapshot.select_default_microphone(Some(10));
    tracker.publish(&state, snapshot.clone());
    let epoch = receiver.borrow().microphone_epoch;
    snapshot.microphone_clients.clear();
    tracker.publish(&state, snapshot);
    assert!(receiver.borrow().microphone);
    assert_eq!(receiver.borrow().microphone_epoch, epoch);
    tracker.invalidate_selection(&state, true);
    assert!(!receiver.borrow().microphone);
}
