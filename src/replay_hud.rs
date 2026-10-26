//! In-game record/replay controls and badge (`F9`, `F10`, `F4`).
//!
//! - **F9** restarts the current site and records every tick; **F9** again (or any restart)
//!   stops and writes `scenarios/rec-<UTC timestamp>.ron`.
//! - **F10** replays the last recording (this session's, else the newest `scenarios/rec-*.ron`)
//!   from a fresh restart, ignoring live drive/upgrade input. When every recorded tick has run
//!   the simulation holds and the badge shows `MATCH` or `DRIFT: <fields>`. **F10** again
//!   leaves the replay and hands control back.
//! - **F4** toggles Avian2D collider outlines in the isometric view.
//!
//! Lives in the binary crate (not `pd_ui`) because it reads `pd_replay`, which sits above
//! `pd_ui` in the crate DAG.

use crate::overlays::ColliderOverlay;
use bevy::prelude::*;
use pd_core::SimTick;
use pd_replay::{
    CaptureMode, ReplayMode, ReplaySession, Scenario, Verdict, begin_recording, begin_replay,
    exit_replay, finish_recording,
};
use std::path::{Path, PathBuf};

/// Directory F9 writes recordings to and F10 falls back to (default `scenarios/`).
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct RecordingDir(pub PathBuf);

impl Default for RecordingDir {
    fn default() -> Self {
        Self(PathBuf::from("scenarios"))
    }
}

/// Marker for the top-center REC / REPLAY badge text.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct ReplayBadgeText;

/// Filename-safe recording name for `time` (`rec-2026-10-25T20-47-00Z.ron`).
#[must_use]
pub fn recording_file_name(time: std::time::SystemTime) -> String {
    format!("rec-{}.ron", pd_replay::utc_timestamp(time).replace(':', "-"))
}

/// Newest `rec-*.ron` in `dir` by name (names sort chronologically).
#[must_use]
pub fn newest_recording(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rec-") && n.ends_with(".ron"))
        })
        .max()
}

fn save_recording(world: &mut World, scenario: &Scenario) {
    let dir = world.get_resource::<RecordingDir>().cloned().unwrap_or_default();
    let mut named = scenario.clone();
    let file = recording_file_name(std::time::SystemTime::now());
    named.name = file.trim_end_matches(".ron").to_string();
    let path = dir.0.join(&file);
    let note = match named.save(&path) {
        Ok(()) => {
            info!("Recorded {} ticks to {}", named.exit_tick, path.display());
            format!("Saved {} ticks to {}", named.exit_tick, path.display())
        }
        Err(e) => {
            warn!("Could not save recording: {e}");
            format!("Recording kept in memory; save failed: {e}")
        }
    };
    if let Some(mut session) = world.get_resource_mut::<ReplaySession>() {
        session.last_recording = Some(named);
        session.note = note;
    }
}

/// `Update`: F9 / F10 / F4 handling (exclusive: starting/stopping needs the whole world).
pub fn replay_hotkeys_system(world: &mut World) {
    let Some(keys) = world.get_resource::<ButtonInput<KeyCode>>() else {
        return;
    };
    let (f9, f10, f4) = (
        keys.just_pressed(KeyCode::F9),
        keys.just_pressed(KeyCode::F10),
        keys.just_pressed(KeyCode::F4),
    );

    if f4 && let Some(mut overlay) = world.get_resource_mut::<ColliderOverlay>() {
        overlay.0 = !overlay.0;
    }

    let (recording, replaying) = world
        .get_resource::<ReplaySession>()
        .map_or((false, false), |s| (s.is_recording(), s.is_replaying()));

    if f9 && !replaying {
        if recording {
            let _ = finish_recording(world);
        } else {
            begin_recording(world);
        }
    }

    // Write any finished recording (F9 above, or auto-stopped by a restart).
    let finished = world
        .get_resource_mut::<ReplaySession>()
        .and_then(|mut s| s.pending_save.take());
    if let Some(scenario) = finished {
        save_recording(world, &scenario);
    }
    if f9 {
        return;
    }

    if f10 && !recording {
        if replaying {
            exit_replay(world);
            return;
        }
        let in_memory = world.get_resource::<ReplaySession>().and_then(|s| s.last_recording.clone());
        let scenario = in_memory.or_else(|| {
            let dir = world.get_resource::<RecordingDir>().cloned().unwrap_or_default();
            newest_recording(&dir.0).and_then(|p| match Scenario::load(&p) {
                Ok(s) => Some(s),
                Err(e) => {
                    warn!("Cannot replay {}: {e}", p.display());
                    None
                }
            })
        });
        match scenario {
            Some(s) => begin_replay(world, s, CaptureMode::Ignore),
            None => {
                if let Some(mut session) = world.get_resource_mut::<ReplaySession>() {
                    session.note = "No recording to replay. Press F9 to record.".to_string();
                }
            }
        }
    }
}

/// Badge text for the current session state (`None` hides the badge).
#[must_use]
pub fn badge_text(session: &ReplaySession, tick: u64, blink_on: bool) -> Option<String> {
    match &session.mode {
        ReplayMode::Recording(_) => Some(format!(
            "{} REC  tick {tick}  [F9 stop]",
            if blink_on { "(*)" } else { "( )" }
        )),
        ReplayMode::Replaying(player) => {
            if let Some(verdict) = session.finished_verdict() {
                Some(format!("REPLAY {}  [F10 exit]", verdict.banner()))
            } else {
                Some(format!(
                    "REPLAY  tick {tick} / {}  [F10 exit]",
                    player.scenario.exit_tick
                ))
            }
        }
        ReplayMode::Idle => None,
    }
}

fn spawn_replay_badge_system(mut commands: Commands, existing: Query<(), With<ReplayBadgeText>>) {
    if existing.iter().next().is_some() {
        return;
    }
    commands.spawn((
        Name::new("Record / Replay Badge"),
        ReplayBadgeText,
        Text::new(""),
        TextFont {
            font_size: 22.0,
            ..Default::default()
        },
        TextColor(Color::srgb(1.0, 0.3, 0.25)),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(64.0),
            left: Val::Percent(36.0),
            ..Default::default()
        },
        Visibility::Hidden,
    ));
}

fn update_replay_badge_system(
    time: Res<Time<Real>>,
    session: Res<ReplaySession>,
    tick: Res<SimTick>,
    mut badge: Query<(&mut Text, &mut TextColor, &mut Visibility), With<ReplayBadgeText>>,
) {
    let Ok((mut text, mut color, mut visibility)) = badge.get_single_mut() else {
        return;
    };
    let blink_on = time.elapsed_secs().fract() < 0.5;
    match badge_text(&session, tick.0, blink_on) {
        Some(label) => {
            color.0 = match session.finished_verdict() {
                Some(Verdict::Match) => Color::srgb(0.35, 0.95, 0.45),
                Some(Verdict::Drift(_)) => Color::srgb(1.0, 0.75, 0.2),
                _ if session.is_recording() => Color::srgb(1.0, 0.3, 0.25),
                _ => Color::srgb(0.6, 0.85, 1.0),
            };
            if text.0 != label {
                text.0 = label;
            }
            *visibility = Visibility::Visible;
        }
        None => *visibility = Visibility::Hidden,
    }
}

/// Registers F9 / F10 / F4 and the REC / REPLAY badge for the windowed game.
pub struct ReplayHudPlugin;

impl Plugin for ReplayHudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RecordingDir>()
            .add_systems(Startup, spawn_replay_badge_system)
            .add_systems(
                Update,
                (replay_hotkeys_system, update_replay_badge_system)
                    .chain()
                    .before(pd_core::PdSystemSet::InputAndClock),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_names_sort_chronologically_and_are_filename_safe() {
        let t0 = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_761_350_400);
        let t1 = t0 + std::time::Duration::from_secs(61);
        let (a, b) = (recording_file_name(t0), recording_file_name(t1));
        assert_eq!(a, "rec-2025-10-25T00-00-00Z.ron");
        assert!(a < b);
        assert!(!a.contains(':'));
    }
}
