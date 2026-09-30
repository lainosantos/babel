//! Portable policy for Windows endpoint pairing and session activity. The OS
//! adapter supplies exact driver metadata and capture-default endpoint IDs,
//! never a guessed GUID or an output-default activity fallback.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::audio::{DeviceDirection, native_ids};
use anyhow::{Context, Result, ensure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Capture,
    Render,
}

#[derive(Clone, Debug)]
pub(super) struct Endpoint {
    pub id: String,
    pub direction: Direction,
    pub description: String,
    pub adapter: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Pair {
    pub selected: String,
    pub opposite: String,
}

fn family(endpoint: &Endpoint) -> Option<&'static str> {
    let direction = match endpoint.direction {
        Direction::Capture => DeviceDirection::Input,
        Direction::Render => DeviceDirection::Output,
    };
    if let Some(cable) =
        native_ids::babel_windows_cable(&endpoint.description, &endpoint.adapter, direction)
    {
        return Some(cable);
    }
    for (family, adapter) in [
        ("CABLE", "VB-Audio Virtual Cable"),
        ("CABLE-A", "VB-Audio Cable A"),
        ("CABLE-B", "VB-Audio Cable B"),
        ("CABLE-C", "VB-Audio Cable C"),
        ("CABLE-D", "VB-Audio Cable D"),
    ] {
        let role = match endpoint.direction {
            Direction::Render => "Input",
            Direction::Capture => "Output",
        };
        if endpoint
            .description
            .trim()
            .eq_ignore_ascii_case(&format!("{family} {role}"))
            && endpoint.adapter.trim().eq_ignore_ascii_case(adapter)
        {
            return Some(family);
        }
    }
    None
}

pub(super) fn pair(endpoints: &[Endpoint], id: &str, direction: Direction) -> Result<Pair> {
    let selected: Vec<_> = endpoints
        .iter()
        .filter(|endpoint| endpoint.id == id && endpoint.direction == direction)
        .collect();
    ensure!(
        selected.len() == 1,
        "the selected Windows virtual endpoint is missing or ambiguous; refresh and select it again"
    );
    let selected = selected[0];
    let family = family(selected).context("automatic Windows activity detection requires an unambiguous Babel Audio v1 or supported VB-CABLE driver pair")?;
    let related: Vec<_> = endpoints
        .iter()
        .filter(|endpoint| self::family(endpoint) == Some(family))
        .collect();
    let same: Vec<_> = related
        .iter()
        .filter(|endpoint| endpoint.direction == direction)
        .collect();
    let opposite: Vec<_> = related
        .iter()
        .filter(|endpoint| endpoint.direction != direction)
        .collect();
    ensure!(
        same.len() == 1 && opposite.len() == 1,
        "the Windows virtual cable has a missing or ambiguous opposite endpoint; routing remains suspended"
    );
    ensure!(
        selected.id != opposite[0].id,
        "Windows virtual cable endpoints must have distinct stable IDs"
    );
    Ok(Pair {
        selected: selected.id.clone(),
        opposite: opposite[0].id.clone(),
    })
}

pub(super) fn independent(microphone: &Pair, speaker: &Pair) -> Result<()> {
    ensure!(
        microphone.selected != speaker.opposite && microphone.opposite != speaker.selected,
        "select two independent Windows virtual cables for microphone and speaker"
    );
    Ok(())
}

/// The microphone feed is a render endpoint, but apps and Windows select its
/// paired capture endpoint. Speaker defaults cannot authorize either route.
pub(super) fn microphone_requested(
    pair: &Pair,
    default_capture_ids: &[String],
    external: bool,
) -> bool {
    external || default_capture_ids.iter().any(|id| id == &pair.opposite)
}

pub(super) fn client_epoch_changed(
    previous_default: bool,
    current_default: bool,
    previous: u64,
    current: u64,
) -> bool {
    // While system selection independently holds the mic open, a capture app
    // opening/closing does not interrupt wake-word audio or reset its queues.
    !(previous_default && current_default) && previous != current
}

pub(super) fn external_process(process_id: u32, babel_process_id: u32) -> bool {
    // Windows uses PID zero for system-sounds sessions; those are external too.
    process_id != babel_process_id
}

pub(super) fn retain_current_sessions<T>(
    sessions: &mut HashMap<String, T>,
    current: &HashSet<String>,
) {
    // A retained control can follow a session when Windows switches streams.
    // Only the fresh enumeration of the selected endpoint authorizes activity.
    sessions.retain(|id, _| current.contains(id));
}

/// Existing-session callbacks preserve fast stop/start edges without doing COM
/// work, taking a mutex, allocating, or forwarding an unbounded event queue.
#[derive(Default)]
pub(super) struct ActivitySignal {
    active: AtomicU64,
    occupied: AtomicU64,
    epoch: AtomicU64,
}
impl ActivitySignal {
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire) != 0
    }
}

pub(super) struct SessionActivity {
    mask: u64,
    pub changes: AtomicU64,
    signal: Arc<ActivitySignal>,
}
impl SessionActivity {
    pub fn active(&self) -> bool {
        self.signal.active.load(Ordering::Acquire) & self.mask != 0
    }
    pub fn new(signal: Arc<ActivitySignal>) -> Result<Arc<Self>> {
        let before = signal
            .occupied
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |bits| {
                (bits != u64::MAX).then(|| bits | (1_u64 << bits.trailing_ones()))
            })
            .map_err(|_| {
                anyhow::anyhow!(
                    "Windows activity monitoring supports at most 64 external sessions per endpoint"
                )
            })?;
        Ok(Arc::new(Self {
            mask: 1_u64 << before.trailing_ones(),
            changes: AtomicU64::new(0),
            signal,
        }))
    }
    pub fn update(&self, active: bool) {
        self.changes.fetch_add(1, Ordering::AcqRel);
        let edge = if active {
            self.signal.active.fetch_or(self.mask, Ordering::AcqRel) == 0
        } else {
            self.signal.active.fetch_and(!self.mask, Ordering::AcqRel) == self.mask
        };
        if edge {
            self.signal.epoch.fetch_add(1, Ordering::AcqRel);
        }
    }
}
impl Drop for SessionActivity {
    fn drop(&mut self) {
        self.update(false);
        self.signal.occupied.fetch_and(!self.mask, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoints() -> Vec<Endpoint> {
        [
            (
                "render-a",
                Direction::Render,
                "CABLE-A Input",
                "VB-Audio Cable A",
            ),
            (
                "capture-a",
                Direction::Capture,
                "CABLE-A Output",
                "VB-Audio Cable A",
            ),
            (
                "render-b",
                Direction::Render,
                "CABLE-B Input",
                "VB-Audio Cable B",
            ),
            (
                "capture-b",
                Direction::Capture,
                "CABLE-B Output",
                "VB-Audio Cable B",
            ),
        ]
        .into_iter()
        .map(|(id, direction, description, adapter)| Endpoint {
            id: id.into(),
            direction,
            description: description.into(),
            adapter: adapter.into(),
        })
        .collect()
    }

    fn babel_endpoints() -> Vec<Endpoint> {
        [
            (
                "babel-mic-render",
                Direction::Render,
                "Babel Microphone Feed",
            ),
            ("babel-mic-capture", Direction::Capture, "Babel Microphone"),
            ("babel-speaker-render", Direction::Render, "Babel Speaker"),
            (
                "babel-speaker-capture",
                Direction::Capture,
                "Babel Speaker Monitor",
            ),
        ]
        .into_iter()
        .map(|(id, direction, description)| Endpoint {
            id: id.into(),
            direction,
            description: description.into(),
            adapter: native_ids::BABEL_WINDOWS_ADAPTER.into(),
        })
        .collect()
    }

    #[test]
    fn babel_pairs_are_independent_and_coexist_with_external_cables() {
        let mut endpoints = babel_endpoints();
        endpoints.extend(self::endpoints());
        endpoints.reverse();
        let mic = pair(&endpoints, "babel-mic-render", Direction::Render).unwrap();
        let speaker = pair(&endpoints, "babel-speaker-capture", Direction::Capture).unwrap();
        assert_eq!(mic.opposite, "babel-mic-capture");
        assert_eq!(speaker.opposite, "babel-speaker-render");
        independent(&mic, &speaker).unwrap();
        independent(
            &mic,
            &pair(&endpoints, "capture-a", Direction::Capture).unwrap(),
        )
        .unwrap();
        assert!(
            independent(
                &mic,
                &pair(&endpoints, "babel-mic-capture", Direction::Capture).unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn babel_requires_exact_driver_roles_and_a_unique_opposite_endpoint() {
        let original = babel_endpoints();
        for (description, adapter) in [
            (
                "Babel Microphone Feed renamed",
                native_ids::BABEL_WINDOWS_ADAPTER,
            ),
            ("Babel Microphone", native_ids::BABEL_WINDOWS_ADAPTER),
            ("Babel Microphone Feed", "Babel Audio v1 copy"),
            ("Babel Microphone Feed", "VB-Audio Cable A"),
        ] {
            let mut endpoints = original.clone();
            endpoints[0].description = description.into();
            endpoints[0].adapter = adapter.into();
            assert!(pair(&endpoints, "babel-mic-render", Direction::Render).is_err());
        }
        let mut missing = original.clone();
        missing.remove(1);
        assert!(pair(&missing, "babel-mic-render", Direction::Render).is_err());
        // Failure of the microphone cable does not invalidate the speaker cable.
        assert!(pair(&missing, "babel-speaker-capture", Direction::Capture).is_ok());
        let mut ambiguous = original.clone();
        ambiguous.push(Endpoint {
            id: "second-mic-capture".into(),
            ..original[1].clone()
        });
        assert!(pair(&ambiguous, "babel-mic-render", Direction::Render).is_err());
        assert!(pair(&ambiguous, "babel-speaker-capture", Direction::Capture).is_ok());
    }

    #[test]
    fn pairs_opposite_sides_and_never_the_other_cable() {
        let endpoints = endpoints();
        let mic = pair(&endpoints, "render-a", Direction::Render).unwrap();
        let speaker = pair(&endpoints, "capture-b", Direction::Capture).unwrap();
        assert_eq!(mic.opposite, "capture-a");
        assert_eq!(speaker.opposite, "render-b");
        independent(&mic, &speaker).unwrap();
        assert!(
            independent(
                &mic,
                &pair(&endpoints, "capture-a", Direction::Capture).unwrap()
            )
            .is_err()
        );
    }
    #[test]
    fn missing_ambiguous_unrecognized_and_wrong_direction_fail_closed() {
        let mut endpoints = endpoints();
        assert!(pair(&endpoints, "unknown", Direction::Render).is_err());
        assert!(pair(&endpoints, "render-a", Direction::Capture).is_err());
        endpoints[1].adapter = "unrelated adapter".into();
        assert!(pair(&endpoints, "render-a", Direction::Render).is_err());
        endpoints[1].adapter = "VB-Audio Cable A".into();
        endpoints.push(Endpoint {
            id: "duplicate-a".into(),
            ..endpoints[1].clone()
        });
        assert!(pair(&endpoints, "render-a", Direction::Render).is_err());
        endpoints[0].description = "not CABLE-A Input".into();
        assert!(pair(&endpoints, "render-a", Direction::Render).is_err());
    }
    #[test]
    fn uses_stable_identity_and_exact_driver_properties_not_enumeration_order() {
        let mut endpoints = endpoints();
        endpoints.reverse();
        assert_eq!(
            pair(&endpoints, "render-a", Direction::Render)
                .unwrap()
                .opposite,
            "capture-a"
        );
        endpoints
            .iter_mut()
            .find(|endpoint| endpoint.id == "render-a")
            .unwrap()
            .adapter = "VB-Audio Cable A spoof".into();
        assert!(pair(&endpoints, "render-a", Direction::Render).is_err());
    }
    #[test]
    fn only_last_client_stopping_and_first_starting_change_route_epoch() {
        let signal = Arc::new(ActivitySignal::default());
        let first = SessionActivity::new(signal.clone()).unwrap();
        let second = SessionActivity::new(signal.clone()).unwrap();
        first.update(true);
        assert!(first.active());
        second.update(true);
        assert!(signal.active());
        assert_eq!(signal.epoch(), 1);
        first.update(false);
        assert!(signal.active());
        assert_eq!(signal.epoch(), 1);
        second.update(false);
        second.update(true);
        assert!(signal.active());
        assert_eq!(signal.epoch(), 3);
        second.update(true);
        assert_eq!(signal.epoch(), 3);
        assert!(second.changes.load(Ordering::Acquire) >= 4);
    }
    #[test]
    fn session_slots_are_bounded_and_reclaimed_after_callbacks_are_dropped() {
        let signal = Arc::new(ActivitySignal::default());
        let mut sessions = Vec::new();
        for _ in 0..64 {
            sessions.push(SessionActivity::new(signal.clone()).unwrap());
        }
        assert!(SessionActivity::new(signal.clone()).is_err());
        sessions[0].update(true);
        assert!(signal.active());
        sessions.remove(0);
        assert!(!signal.active());
        let replacement = SessionActivity::new(signal.clone()).unwrap();
        replacement.update(true);
        assert!(signal.active());
        drop(replacement);
        assert!(!signal.active());
    }
    #[test]
    fn concurrent_session_updates_cannot_underflow_or_leave_a_stale_active_route() {
        let signal = Arc::new(ActivitySignal::default());
        let session = SessionActivity::new(signal.clone()).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let session = session.clone();
                scope.spawn(move || {
                    for _ in 0..1000 {
                        session.update(true);
                        session.update(false);
                    }
                });
            }
        });
        session.update(false);
        assert!(!signal.active());
    }
    #[test]
    fn moving_a_session_off_the_cable_drops_its_activity_even_without_expired_event() {
        let signal = Arc::new(ActivitySignal::default());
        let activity = SessionActivity::new(signal.clone()).unwrap();
        activity.update(true);
        let mut sessions = HashMap::from([("virtual-session".into(), activity)]);
        assert!(signal.active());
        retain_current_sessions(&mut sessions, &HashSet::from(["physical-session".into()]));
        assert!(sessions.is_empty());
        assert!(!signal.active());
        let new_virtual = SessionActivity::new(signal.clone()).unwrap();
        new_virtual.update(true);
        sessions.insert("new-virtual-session".into(), new_virtual);
        retain_current_sessions(
            &mut sessions,
            &HashSet::from(["new-virtual-session".into()]),
        );
        assert!(signal.active());
        assert_eq!(signal.epoch(), 3);
    }
    #[test]
    fn excludes_only_babel_process_and_keeps_system_sounds_external() {
        assert!(!external_process(123, 123));
        assert!(external_process(456, 123));
        assert!(external_process(0, 123));
    }
    #[test]
    fn system_capture_default_matches_paired_capture_id_and_keeps_app_authorization() {
        let mic = pair(&babel_endpoints(), "babel-mic-render", Direction::Render).unwrap();
        assert!(microphone_requested(
            &mic,
            &["babel-mic-capture".into()],
            false
        ));
        assert!(microphone_requested(
            &mic,
            &["physical-capture".into(), "babel-mic-capture".into()],
            false
        ));
        for defaults in [
            vec![],
            vec!["physical-capture".into()],
            vec!["babel-mic-render".into()],
            vec!["babel-speaker-capture".into()],
            vec!["babel-mic-capture-copy".into()],
        ] {
            assert!(!microphone_requested(&mic, &defaults, false));
            assert!(microphone_requested(&mic, &defaults, true));
        }
    }
    #[test]
    fn stable_system_mic_ignores_client_epoch_but_other_routes_keep_edges() {
        assert!(!client_epoch_changed(true, true, 1, 3));
        assert!(client_epoch_changed(false, false, 1, 3));
        assert!(client_epoch_changed(true, false, 1, 3));
        assert!(client_epoch_changed(false, true, 1, 3));
        assert!(!client_epoch_changed(false, false, 3, 3));
    }
}
