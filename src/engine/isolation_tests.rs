use super::*;

#[test]
fn device_deselection_revokes_audio_while_the_processing_worker_is_blocked() {
    // Private runtimes keep deliberate worker starvation out of other tests.
    let processing = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let control = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    control.block_on(async {
        let (usage, current) = watch::channel(audio::activity::EndpointUse {
            microphone: true,
            ..Default::default()
        });
        let cancel = CancellationToken::new();
        let metrics = Arc::new(RouteMetrics::default());
        let (opened, active) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let mut opened = Some(opened);
        let mut blocked = Some(blocked);
        let processing = processing.handle().clone();
        let gate_cancel = cancel.clone();
        let gate_metrics = metrics.clone();
        let gate = tokio::spawn(async move {
            activity::while_selected(
                current,
                TranscriptOrigin::Microphone,
                gate_metrics,
                gate_cancel,
                |active_cancel| {
                    let opened = opened.take().unwrap();
                    let blocked = blocked.take().unwrap();
                    let processing = processing.clone();
                    async move {
                        processing_route(&processing, async move {
                            opened.send(active_cancel).unwrap();
                            // Simulate an uncooperative model occupying the sole
                            // processing worker; only the control gate can run.
                            let _ = blocked.recv();
                            Ok(())
                        })
                        .await
                    }
                },
            )
            .await
        });
        let active = tokio::time::timeout(Duration::from_secs(2), active)
            .await
            .unwrap()
            .unwrap();
        usage.send_modify(|usage| {
            usage.microphone = false;
            usage.microphone_epoch += 1;
        });
        // Cancellation is also what the dedicated device workers observe.
        tokio::time::timeout(Duration::from_secs(1), active.cancelled())
            .await
            .expect("OS deselection was delayed by processing");
        assert!(metrics.audio.playback_generation.load(Ordering::Acquire) > 0);
        release.send(()).unwrap();
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), gate)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    });
}

#[test]
fn dropping_the_control_waiter_aborts_its_processing_route() {
    let processing = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let control = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    control.block_on(async {
        let ended = CancellationToken::new();
        let dropped = ended.clone();
        let handle = processing.handle().clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            processing_route(&handle, async move {
                let _finished = dropped.drop_guard();
                started.send(()).unwrap();
                std::future::pending::<Result<()>>().await
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        waiter.abort();
        let _ = waiter.await;
        tokio::time::timeout(Duration::from_secs(1), ended.cancelled())
            .await
            .expect("processing route was detached after its gate disappeared");
    });
}
