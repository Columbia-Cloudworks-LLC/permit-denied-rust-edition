//! Ranch benchmark gates (Rust equivalent of the reference's ranch playthrough test in
//! `src/game/Game.test.ts`): record the canonical ranch demolition from live input, then
//! replay it to the same end state without live input; the committed scenario replays to MATCH.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use bevy::prelude::*;
use pd_core::{AppState, EnvironmentConfig, GameMode, SimTick, SiteTestMap};
use pd_dozer::{LiveDriveControl, UpgradeChoice, UpgradePickQueue};
use pd_replay::{
    CaptureMode, PdReplayPlugin, ReplaySession, Scenario, ScenarioAction, Verdict, begin_recording,
    begin_replay, finish_recording,
};
use permit_denied::scenario_cli::run_headless;
use std::path::Path;

/// Live keys at a tick, as the reference test presses them: W 150, `1` + Space 20,
/// A (with W) 40, W 60, release 30.
fn live_keys(tick: u64) -> LiveDriveControl {
    let (left_track, right_track, blade_pressed) = match tick {
        0..150 => (1.0, 1.0, false),
        150..170 => (1.0, 1.0, true),
        // W+A: world-space tracks, so the screen-left turn is left = 2, right = 0
        // (see `pd_dozer::screen_tracks_to_world_tracks`).
        170..210 => (2.0, 0.0, false),
        210..270 => (1.0, 1.0, false),
        _ => (0.0, 0.0, false),
    };
    LiveDriveControl { left_track, right_track, blade_pressed }
}

#[test]
fn ranch_records_from_live_input_then_replays_to_the_same_end_state() {
    let mut app = permit_denied::headless_sim_app();
    app.add_plugins(PdReplayPlugin);
    app.insert_resource(SiteTestMap::Ranch);
    app.world_mut().resource_mut::<EnvironmentConfig>().mode = GameMode::Sandbox;
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Playing);
    app.update();

    begin_recording(app.world_mut());
    let mut frames = 0;
    while app.world().resource::<SimTick>().0 < 300 || frames < 2 {
        frames += 1;
        assert!(frames < 1_000);
        let tick = app.world().resource::<SimTick>().0;
        *app.world_mut().resource_mut::<LiveDriveControl>() = live_keys(tick);
        if tick == 150 {
            app.world_mut().resource_mut::<UpgradePickQueue>().push(UpgradeChoice::Ar400Edge);
        }
        app.update();
    }
    let recorded = finish_recording(app.world_mut()).unwrap();

    assert!(recorded.exit_tick > 250);
    assert_eq!(recorded.settings.test_map, SiteTestMap::Ranch);
    assert_eq!(
        recorded.actions,
        vec![ScenarioAction::Upgrade { at: 150, choice: UpgradeChoice::Ar400Edge }]
    );
    let digest = recorded.expect.clone().unwrap();
    assert!(digest.cash > 0);
    let total: u32 = digest.cells.values().sum();
    assert!(digest.cells.get("intact").copied().unwrap_or(0) < total, "ranch was damaged");

    // Replay in place while hammering live keys and upgrade picks: neither may reach the run.
    begin_replay(app.world_mut(), recorded.clone(), CaptureMode::Ignore);
    for _ in 0..1_000 {
        *app.world_mut().resource_mut::<LiveDriveControl>() =
            LiveDriveControl { left_track: -1.0, right_track: -1.0, blade_pressed: true };
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::Digit2);
        app.update();
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        if app.world().resource::<ReplaySession>().finished_verdict().is_some() {
            break;
        }
    }
    assert_eq!(app.world().resource::<ReplaySession>().finished_verdict(), Some(&Verdict::Match));

    // And in a fresh process-equivalent app.
    assert_eq!(run_headless(recorded).unwrap().result.verdict, Verdict::Match);
}

#[test]
fn committed_ranch_scenario_replays_to_match() {
    let scenario = Scenario::load(Path::new("scenarios/ranch.ron")).unwrap();
    assert_eq!(scenario.settings.test_map, SiteTestMap::Ranch);
    let run = run_headless(scenario).unwrap();
    assert_eq!(run.result.verdict, Verdict::Match, "{:?}", run.result.verdict);
    assert_eq!(run.captures.len(), 9);
    assert!(run.captures.first().is_some_and(|c| c.digest.cells.get("destroyed") == Some(&0)));
}
