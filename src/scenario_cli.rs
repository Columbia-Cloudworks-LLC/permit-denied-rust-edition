//! Scenario runner: `cargo run -- --scenario <file.ron> [--out <dir>] [--headless]
//! [--write-expect]`.
//!
//! Runs a `permit-denied-scenario` in lockstep (exactly one 60 Hz tick per frame), so every
//! frame, camera position and capture is reproducible. At each capture tick the simulation
//! freezes, a PNG and `<tick>-<label>.telemetry.json` are written, then it resumes. At the exit
//! tick the end state is judged against `expect` (`MATCH` / `DRIFT`), a labeled
//! `contact-sheet.png` and `summary.json` are written, and the process exits 0 on MATCH or
//! no-expectation, 1 on DRIFT and 2 on errors. `--headless` skips the window and PNGs.

use crate::contact_sheet::{SheetFrame, SheetLayout, compose_contact_sheet};
use crate::overlays::ColliderOverlay;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use pd_core::DebugTelemetry;
use pd_destruction::CollapseTaskJoinStats;
use pd_replay::{
    CaptureMode, CaptureTelemetry, PdReplayPlugin, ReplayResult, ReplaySession, Scenario, Verdict,
    begin_replay, complete_pending_capture,
};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Parsed command line for a scenario run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArgs {
    pub scenario: PathBuf,
    pub out: Option<PathBuf>,
    pub headless: bool,
    pub write_expect: bool,
}

/// Parses `args` (without the program name). `Ok(None)` when no `--scenario` was given.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<CliArgs>, String> {
    let mut scenario = None;
    let mut out = None;
    let mut headless = false;
    let mut write_expect = false;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--scenario" | "--replay" => {
                scenario = Some(PathBuf::from(it.next().ok_or("--scenario needs a file path")?));
            }
            "--out" => out = Some(PathBuf::from(it.next().ok_or("--out needs a directory")?)),
            "--headless" => headless = true,
            "--write-expect" => write_expect = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    match scenario {
        Some(scenario) => Ok(Some(CliArgs {
            scenario,
            out,
            headless,
            write_expect,
        })),
        None if out.is_some() || headless || write_expect => {
            Err("--out/--headless/--write-expect need --scenario <file>".to_string())
        }
        None => Ok(None),
    }
}

/// Result of a headless run.
pub struct HeadlessRun {
    pub result: ReplayResult,
    pub captures: Vec<CaptureTelemetry>,
    pub collapse_joins: CollapseTaskJoinStats,
    pub app: App,
}

/// Runs `scenario` in a headless lockstep app until its exit tick and returns the verdict.
pub fn run_headless(scenario: Scenario) -> Result<HeadlessRun, String> {
    let mut app = crate::headless_sim_app();
    app.add_plugins(PdReplayPlugin);
    let budget = scenario.exit_tick.saturating_add(600);
    begin_replay(app.world_mut(), scenario, CaptureMode::DigestOnly);
    for _ in 0..budget {
        app.update();
        if let Some(result) = finished_result(app.world()) {
            let session = app.world().resource::<ReplaySession>();
            let captures = session.captures.clone();
            let collapse_joins = *app.world().resource::<CollapseTaskJoinStats>();
            return Ok(HeadlessRun {
                result,
                captures,
                collapse_joins,
                app,
            });
        }
    }
    Err(format!("scenario did not finish within {budget} frames"))
}

fn finished_result(world: &World) -> Option<ReplayResult> {
    let session = world.get_resource::<ReplaySession>()?;
    session.finished_verdict()?;
    session.last_result.clone()
}

fn slug(label: &str) -> String {
    let s: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let trimmed = s.trim_matches('-');
    if trimmed.is_empty() { "capture".to_string() } else { trimmed.to_string() }
}

fn capture_stem(capture: &CaptureTelemetry) -> String {
    format!("{:06}-{}", capture.tick, slug(&capture.label))
}

fn caption(capture: &CaptureTelemetry) -> String {
    format!("{} - T={} ({:.2}S)", capture.label, capture.tick, capture.sim_secs)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

#[derive(Serialize)]
struct Summary<'a> {
    scenario: &'a str,
    build: &'a str,
    verdict: String,
    drift: Vec<String>,
    digest: &'a pd_replay::Digest,
    collapse_joins: CollapseTaskJoinStats,
    captures: Vec<String>,
    contact_sheet: Option<String>,
}

fn verdict_exit(verdict: &Verdict) -> AppExit {
    match verdict {
        Verdict::Drift(_) => AppExit::from_code(1),
        Verdict::Match | Verdict::Unverified => AppExit::Success,
    }
}

fn finish_outputs(
    scenario: &Scenario,
    args: &CliArgs,
    out_dir: &Path,
    result: &ReplayResult,
    captures: &[CaptureTelemetry],
    collapse_joins: CollapseTaskJoinStats,
    sheet: Option<PathBuf>,
) -> Result<(), String> {
    for capture in captures {
        write_json(&out_dir.join(format!("{}.telemetry.json", capture_stem(capture))), capture)?;
    }
    let drift = match &result.verdict {
        Verdict::Drift(fields) => fields.clone(),
        _ => Vec::new(),
    };
    let summary = Summary {
        scenario: &scenario.name,
        build: &scenario.build,
        verdict: result.verdict.banner(),
        drift,
        digest: &result.digest,
        collapse_joins,
        captures: captures.iter().map(capture_stem).collect(),
        contact_sheet: sheet.map(|p| p.display().to_string()),
    };
    write_json(&out_dir.join("summary.json"), &summary)?;
    if args.write_expect {
        let mut updated = scenario.clone();
        updated.expect = Some(result.digest.clone());
        updated.build = pd_replay::build_id();
        updated.save(&args.scenario).map_err(|e| e.to_string())?;
        println!("wrote expected digest to {}", args.scenario.display());
    }
    println!(
        "{}  ({} ticks, cash ${}, {} captures, BFS joins {} / blocked {} / max wait {} us)",
        result.verdict.banner(),
        result.digest.ticks,
        result.digest.cash,
        captures.len(),
        collapse_joins.joins,
        collapse_joins.blocked_joins,
        collapse_joins.max_block_micros
    );
    println!("output: {}", out_dir.display());
    Ok(())
}

/// Entry point for `--scenario`: loads the file and runs it headless or windowed.
#[must_use]
pub fn run_cli(args: CliArgs) -> AppExit {
    let scenario = match Scenario::load(&args.scenario) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return AppExit::from_code(2);
        }
    };
    let out_dir = args.out.clone().unwrap_or_else(|| {
        let stem = args.scenario.file_stem().map_or_else(|| "scenario".into(), |s| s.to_string_lossy().into_owned());
        PathBuf::from("target").join("scenario-out").join(stem)
    });
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("cannot create {}: {e}", out_dir.display());
        return AppExit::from_code(2);
    }
    if args.headless {
        return match run_headless(scenario.clone()) {
            Ok(run) => match finish_outputs(&scenario, &args, &out_dir, &run.result, &run.captures, run.collapse_joins, None) {
                Ok(()) => verdict_exit(&run.result.verdict),
                Err(e) => {
                    eprintln!("{e}");
                    AppExit::from_code(2)
                }
            },
            Err(e) => {
                eprintln!("{e}");
                AppExit::from_code(2)
            }
        };
    }
    run_windowed(scenario, args, out_dir)
}

/// Windowed run state shared by the capture systems and the screenshot observer.
#[derive(Resource)]
struct ScenarioRun {
    scenario: Scenario,
    args: CliArgs,
    out_dir: PathBuf,
    started: bool,
    settle_frames: u32,
    shot_requested: bool,
    shot_saved: Option<Result<RgbaFrame, String>>,
    frames: Vec<SheetFrame>,
    shots_taken: u32,
    done: bool,
}

type RgbaFrame = image::RgbaImage;

/// Frames to wait (sim frozen) before taking a screenshot, so transforms, meshes and the HUD
/// reflect the capture tick. Fixed, so camera smoothing is identical on every run.
const SETTLE_FRAMES: u32 = 3;

/// Settle frames for the first capture: Bevy compiles render pipelines asynchronously the
/// first time the site's meshes are drawn, so an early capture would show only the HUD.
/// Extra frozen frames cannot change the simulation (`capture_freezes_do_not_perturb_the_run`).
const FIRST_CAPTURE_SETTLE_FRAMES: u32 = 45;

fn run_windowed(scenario: Scenario, args: CliArgs, out_dir: PathBuf) -> AppExit {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: format!("PERMIT DENIED — scenario {}", scenario.name),
            resolution: (1280.0_f32, 720.0_f32).into(),
            present_mode: bevy::window::PresentMode::AutoNoVsync,
            resizable: false,
            ..Default::default()
        }),
        ..Default::default()
    }))
    .insert_resource(crate::lockstep_time_strategy());
    crate::add_simulation_plugins(&mut app);
    crate::add_presentation_plugins(&mut app);
    app.add_plugins((PdReplayPlugin, crate::overlays::DebugOverlaysPlugin))
        .insert_resource(ColliderOverlay(scenario.overlays.colliders))
        .insert_resource(ScenarioRun {
            scenario,
            args,
            out_dir,
            started: false,
            settle_frames: 0,
            shot_requested: false,
            shot_saved: None,
            frames: Vec::new(),
            shots_taken: 0,
            done: false,
        })
        .add_systems(
            Update,
            (start_scenario_once_system, drive_screenshot_captures_system)
                .chain()
                .after(pd_replay::replay_boundary_update_system),
        );
    app.run()
}

fn start_scenario_once_system(world: &mut World) {
    let scenario = {
        let Some(mut run) = world.get_resource_mut::<ScenarioRun>() else {
            return;
        };
        if run.started {
            return;
        }
        run.started = true;
        run.scenario.clone()
    };
    if let Some(mut debug) = world.get_resource_mut::<DebugTelemetry>() {
        debug.sound_enabled = false;
    }
    begin_replay(world, scenario, CaptureMode::Screenshot);
}

fn drive_screenshot_captures_system(world: &mut World) {
    let pending = world
        .get_resource::<ReplaySession>()
        .and_then(|s| s.pending_screenshot.clone());

    if let Some(capture) = pending {
        let Some(mut run) = world.get_resource_mut::<ScenarioRun>() else {
            return;
        };
        if let Some(saved) = run.shot_saved.take() {
            run.shot_requested = false;
            run.settle_frames = 0;
            match saved {
                Ok(image) => run.frames.push(SheetFrame {
                    image,
                    caption: caption(&capture),
                }),
                Err(e) => error!("capture {:?} failed: {e}", capture.label),
            }
            complete_pending_capture(world);
            return;
        }
        if run.shot_requested {
            return;
        }
        run.settle_frames = run.settle_frames.saturating_add(1);
        let settle = if run.shots_taken == 0 { FIRST_CAPTURE_SETTLE_FRAMES } else { SETTLE_FRAMES };
        if run.settle_frames < settle {
            return;
        }
        run.shot_requested = true;
        run.shots_taken = run.shots_taken.saturating_add(1);
        let path = run.out_dir.join(format!("{}.png", capture_stem(&capture)));
        world
            .spawn(Screenshot::primary_window())
            .observe(move |trigger: Trigger<ScreenshotCaptured>, mut run: ResMut<ScenarioRun>| {
                let saved = trigger
                    .event()
                    .0
                    .clone()
                    .try_into_dynamic()
                    .map_err(|e| e.to_string())
                    .and_then(|img| {
                        // Drop alpha (HDR brightness) like Bevy's save_to_disk, then save.
                        let rgb = img.to_rgb8();
                        rgb.save(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                        Ok(image::DynamicImage::ImageRgb8(rgb).to_rgba8())
                    });
                run.shot_saved = Some(saved);
            });
        return;
    }

    let Some(result) = finished_result(world) else {
        return;
    };
    let (captures, collapse_joins) = (
        world.resource::<ReplaySession>().captures.clone(),
        *world.resource::<CollapseTaskJoinStats>(),
    );
    let Some(mut run) = world.get_resource_mut::<ScenarioRun>() else {
        return;
    };
    if run.done {
        return;
    }
    run.done = true;
    let title = format!(
        "{} - SEED {} - {} TICKS - {}",
        run.scenario.name, run.scenario.seed, result.digest.ticks, result.verdict.banner()
    );
    let frames = std::mem::take(&mut run.frames);
    let sheet_path = run.out_dir.join("contact-sheet.png");
    let sheet = (!frames.is_empty()).then(|| compose_contact_sheet(&title, &frames, SheetLayout::default()));
    let sheet_written = match sheet {
        Some(img) => match img.save(&sheet_path) {
            Ok(()) => Some(sheet_path),
            Err(e) => {
                error!("cannot write contact sheet: {e}");
                None
            }
        },
        None => None,
    };
    let exit = match finish_outputs(&run.scenario, &run.args, &run.out_dir, &result, &captures, collapse_joins, sheet_written) {
        Ok(()) => verdict_exit(&result.verdict),
        Err(e) => {
            eprintln!("{e}");
            AppExit::from_code(2)
        }
    };
    world.send_event(exit);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Option<CliArgs>, String> {
        parse_args(list.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn cli_parsing() -> Result<(), String> {
        assert_eq!(args(&[])?, None);
        let parsed = args(&["--scenario", "scenarios/ranch.ron", "--out", "o", "--headless"])?;
        assert_eq!(
            parsed,
            Some(CliArgs {
                scenario: PathBuf::from("scenarios/ranch.ron"),
                out: Some(PathBuf::from("o")),
                headless: true,
                write_expect: false,
            })
        );
        assert!(args(&["--scenario"]).is_err());
        assert!(args(&["--bogus"]).is_err());
        assert!(args(&["--headless"]).is_err());
        assert_eq!(slug("Mid-collapse (sag)"), "mid-collapse--sag");
        assert_eq!(slug("!!!"), "capture");
        Ok(())
    }
}
