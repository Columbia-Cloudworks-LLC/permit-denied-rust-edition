//! F9 / F10 / F4 in-game record/replay flow, driven through `ButtonInput` like a player.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use bevy::prelude::*;
use pd_core::{AppState, CampaignLevel, EnvironmentConfig, GameMode, InputAuthority, SimTick, SiteRestartRequested};
use pd_dozer::LiveDriveControl;
use pd_replay::{PdReplayPlugin, ReplaySession, Scenario, Verdict};
use permit_denied::overlays::ColliderOverlay;
use permit_denied::replay_hud::{RecordingDir, ReplayBadgeText, ReplayHudPlugin, newest_recording};

fn press(app: &mut App, key: KeyCode) {
    app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(key);
    app.update();
    let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
    keys.release(key);
    keys.clear();
}

fn drive(app: &mut App, until_tick: u64) {
    let mut guard = 0;
    while app.world().resource::<SimTick>().0 < until_tick {
        guard += 1;
        assert!(guard < 3_000, "sim stalled");
        let tick = app.world().resource::<SimTick>().0;
        *app.world_mut().resource_mut::<LiveDriveControl>() = if tick < 48 {
            LiveDriveControl { left_track: -1.0, right_track: 1.0, blade_pressed: false }
        } else {
            LiveDriveControl { left_track: 1.0, right_track: 1.0, blade_pressed: tick > 110 }
        };
        app.update();
    }
}

fn badge(app: &mut App) -> String {
    let world = app.world_mut();
    let mut q = world.query_filtered::<&Text, With<ReplayBadgeText>>();
    q.iter(world).next().map(|t| t.0.clone()).unwrap_or_default()
}

fn app_with_hud(dir: &std::path::Path) -> App {
    let mut app = permit_denied::headless_sim_app();
    app.add_plugins((PdReplayPlugin, ReplayHudPlugin))
        .init_resource::<ColliderOverlay>()
        .insert_resource(RecordingDir(dir.to_path_buf()));
    {
        let mut env = app.world_mut().resource_mut::<EnvironmentConfig>();
        env.mode = GameMode::Sandbox;
        env.level = CampaignLevel::Township3;
    }
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Playing);
    app.world_mut().send_event(SiteRestartRequested { preserve_layout: false });
    app.update();
    drive(&mut app, 30); // some prior play before recording
    app
}

#[test]
fn f9_records_to_disk_f10_replays_to_match_and_exits() {
    let dir = std::env::temp_dir().join(format!("pd-rec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut app = app_with_hud(&dir);

    press(&mut app, KeyCode::F4);
    assert!(app.world().resource::<ColliderOverlay>().0, "F4 toggles collider overlay");

    press(&mut app, KeyCode::F9);
    assert!(app.world().resource::<ReplaySession>().is_recording());
    assert!(badge(&mut app).contains("REC"), "badge: {}", badge(&mut app));
    drive(&mut app, 260);
    press(&mut app, KeyCode::F9);
    assert!(!app.world().resource::<ReplaySession>().is_recording());

    let file = newest_recording(&dir).expect("F9 wrote a recording");
    let saved = Scenario::load(&file).unwrap();
    assert!(saved.exit_tick >= 260 && saved.expect.is_some());

    // Live keys during replay must be ignored.
    press(&mut app, KeyCode::F10);
    assert_eq!(*app.world().resource::<InputAuthority>(), InputAuthority::Replay);
    for _ in 0..1_000 {
        *app.world_mut().resource_mut::<LiveDriveControl>() =
            LiveDriveControl { left_track: -2.0, right_track: 2.0, blade_pressed: true };
        app.update();
        if app.world().resource::<ReplaySession>().finished_verdict().is_some() {
            break;
        }
    }
    let verdict = app.world().resource::<ReplaySession>().finished_verdict().cloned();
    assert_eq!(verdict, Some(Verdict::Match));
    app.update();
    assert!(badge(&mut app).contains("MATCH"), "badge: {}", badge(&mut app));

    press(&mut app, KeyCode::F10);
    assert!(!app.world().resource::<ReplaySession>().is_replaying());
    assert_eq!(*app.world().resource::<InputAuthority>(), InputAuthority::Live);
    let tick = app.world().resource::<SimTick>().0;
    drive(&mut app, tick + 5); // sim runs again under live control
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restart_during_recording_stops_and_saves_it() {
    let dir = std::env::temp_dir().join(format!("pd-rec-b-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut app = app_with_hud(&dir);
    press(&mut app, KeyCode::F9);
    drive(&mut app, 90);
    press(&mut app, KeyCode::KeyB); // Restart Site
    for _ in 0..3 {
        app.update();
    }
    assert!(!app.world().resource::<ReplaySession>().is_recording());
    let saved = Scenario::load(&newest_recording(&dir).expect("auto-saved")).unwrap();
    // The frame that reads B still runs its fixed tick first (90 -> 91); the recording ends
    // with that tick, before the restart tears the site down.
    assert_eq!(saved.exit_tick, 91, "recording ends at the last tick before the restart");
    assert_eq!(saved.expect.as_ref().map(|d| d.ticks), Some(91));
    let _ = std::fs::remove_dir_all(&dir);
}
