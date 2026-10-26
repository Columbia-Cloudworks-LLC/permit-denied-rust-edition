//! Record/replay gates: scenario files replay to MATCH, capture freezes cannot perturb a run,
//! and a recording made from live input replays to MATCH in a fresh app and in place.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use bevy::prelude::*;
use pd_core::{AppState, CampaignLevel, EnvironmentConfig, GameMode, SimTick};
use pd_dozer::{LiveDriveControl, UpgradeChoice, UpgradePickQueue};
use pd_replay::{
    CaptureMode, PdReplayPlugin, ReplaySession, Scenario, ScenarioAction, Verdict, begin_recording,
    begin_replay, complete_pending_capture, digest_drift, finish_recording,
};
use permit_denied::scenario_cli::run_headless;
use std::path::Path;

fn smoke() -> Scenario {
    Scenario::load(Path::new("scenarios/smoke-borough.ron")).expect("smoke scenario loads")
}

#[test]
fn smoke_scenario_replays_to_match() {
    let run = run_headless(smoke()).unwrap();
    assert_eq!(run.result.verdict, Verdict::Match, "{:?}", run.result.verdict);
    assert_eq!(run.captures.len(), 4);
    assert_eq!(run.captures.iter().map(|c| c.tick).collect::<Vec<_>>(), vec![0, 200, 400, 640]);
}

/// Screenshot captures hold the sim for several frames mid-run (physics clock paused, Update
/// systems still running). That must not change a single bit of the outcome.
#[test]
fn capture_freezes_do_not_perturb_the_run() {
    let reference = run_headless(smoke()).unwrap().result.digest;
    for idle in [1_u32, 5] {
        let mut app = permit_denied::headless_sim_app();
        app.add_plugins(PdReplayPlugin);
        begin_replay(app.world_mut(), smoke(), CaptureMode::Screenshot);
        let mut waited = 0;
        let mut finished = None;
        for _ in 0..2_000 {
            app.update();
            if app.world().resource::<ReplaySession>().pending_screenshot.is_some() {
                waited += 1;
                if waited >= idle {
                    complete_pending_capture(app.world_mut());
                    waited = 0;
                }
            }
            if let Some(result) = app.world().resource::<ReplaySession>().last_result.clone() {
                finished = Some(result);
                break;
            }
        }
        let result = finished.expect("replay finished");
        assert_eq!(digest_drift(&reference, &result.digest), Vec::<String>::new(), "idle {idle}");
    }
}

/// Drives a live Sandbox session through the recorder: keyboard-equivalent control changes
/// every few ticks plus two upgrade hotkey picks, then stops the recording.
fn record_live_session(app: &mut App) -> Scenario {
    {
        let mut env = app.world_mut().resource_mut::<EnvironmentConfig>();
        env.mode = GameMode::Sandbox;
        env.level = CampaignLevel::Suburb4;
    }
    app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Playing);
    app.update();
    begin_recording(app.world_mut());
    let script: &[(u64, f32, f32, bool)] = &[
        (0, -1.0, 1.0, false),
        (48, 1.0, 1.0, false),
        (110, 1.0, 1.0, true),
        (250, -1.0, 1.0, true),
        (290, 1.0, 1.0, false),
        (420, 0.0, 0.0, false),
    ];
    let mut frames = 0;
    while app.world().resource::<SimTick>().0 < 450 || frames < 3 {
        frames += 1;
        assert!(frames < 2_000, "recording stalled");
        let tick = app.world().resource::<SimTick>().0;
        let ctl = script.iter().rev().find(|c| c.0 <= tick).copied().unwrap();
        *app.world_mut().resource_mut::<LiveDriveControl>() = LiveDriveControl {
            left_track: ctl.1,
            right_track: ctl.2,
            blade_pressed: ctl.3,
        };
        if tick == 60 || tick == 300 {
            let choice = if tick == 60 { UpgradeChoice::Ar400Edge } else { UpgradeChoice::AuxRam };
            app.world_mut().resource_mut::<UpgradePickQueue>().push(choice);
        }
        app.update();
    }
    finish_recording(app.world_mut()).expect("recording finished")
}

#[test]
fn live_recording_replays_to_match_fresh_and_in_place() {
    let mut app = permit_denied::headless_sim_app();
    app.add_plugins(PdReplayPlugin);
    let recording = record_live_session(&mut app);
    assert_eq!(recording.exit_tick, 450);
    assert!(recording.controls.len() >= 6, "control changes recorded: {:?}", recording.controls);
    assert_eq!(
        recording.actions.iter().map(ScenarioAction::at).collect::<Vec<_>>(),
        vec![60, 300],
        "upgrade picks recorded at their ticks"
    );
    assert!(recording.expect.as_ref().is_some_and(|d| d.cash > 0), "the run demolished something");

    // Round-trip through RON exactly as F9 writes it.
    let text = recording.to_ron_string().unwrap();
    let reloaded = Scenario::from_ron_str(&text).unwrap();
    assert_eq!(reloaded, recording);

    let fresh = run_headless(reloaded.clone()).unwrap();
    assert_eq!(fresh.result.verdict, Verdict::Match, "fresh replay: {:?}", fresh.result.verdict);

    // In-place replay (F10) in the app that just recorded: different entity/archetype history.
    begin_replay(app.world_mut(), reloaded, CaptureMode::Ignore);
    for _ in 0..1_000 {
        app.update();
        if app.world().resource::<ReplaySession>().finished_verdict().is_some() {
            break;
        }
    }
    let verdict = app.world().resource::<ReplaySession>().finished_verdict().cloned();
    assert_eq!(verdict, Some(Verdict::Match));
}
