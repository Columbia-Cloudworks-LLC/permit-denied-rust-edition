//! `pd_replay`: deterministic record / replay for Permit Denied.
//!
//! A [`Scenario`] (`permit-denied-scenario` v1, RON) holds the setup that rebuilds a site, the
//! drive control per simulation tick (change entries only), tick-stamped upgrade picks,
//! capture ticks, overlay flags, an exit tick and optionally the expected end-state
//! [`Digest`]. Recording captures exactly what the simulation consumed at each tick
//! (`TrackInput` after `pd_dozer::latch_drive_control_system`, `UpgradeApplied`), so a replay
//! on the same build and OS reproduces the run bit-for-bit and reports `MATCH` or `DRIFT`.
//!
//! Layer 4 of the crate DAG: reads `pd_core`, `pd_world`, `pd_dozer` and `pd_destruction`.

pub mod digest;
pub mod scenario;

pub use digest::{Digest, DozerDigest, compute_digest, digest_drift};
pub use scenario::{
    CaptureSpec, ControlEntry, DriveControl, MAX_SCENARIO_TICKS, Overlays, SCENARIO_FORMAT,
    SCENARIO_VERSION, Scenario, ScenarioAction, ScenarioError, ScenarioSettings, build_id,
};

use bevy::prelude::*;
use pd_core::{
    AppState, DebugTelemetry, EnvironmentConfig, InputAuthority, PdSystemSet, RestartGate,
    SessionEpoch, SimFreeze, SimTick, SiteRestartRequested, SiteSeed, SiteSeedOverride,
    SiteTestMap,
};
use pd_destruction::CollapseTaskJoinStats;
use pd_dozer::{TrackInput, UpgradeApplied, UpgradeMultipliers, UpgradePickQueue, UpgradeProgression};
use serde::Serialize;

/// How a replay handles capture ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureMode {
    /// Record the digest/telemetry in memory and keep running (headless runs, tests).
    #[default]
    DigestOnly,
    /// Also freeze until the presentation layer has saved a screenshot (CLI runner).
    Screenshot,
    /// Ignore capture ticks (in-app replay of a recording).
    Ignore,
}

/// Telemetry written next to each capture.
#[derive(Debug, Clone, Serialize)]
pub struct CaptureTelemetry {
    pub label: String,
    pub tick: u64,
    pub sim_secs: f32,
    pub digest: Digest,
    pub debug: DebugTelemetry,
    pub collapse_joins: CollapseTaskJoinStats,
}

/// Outcome of a finished replay.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Match,
    /// Fields whose end-state values differ from `expect`.
    Drift(Vec<String>),
    /// The scenario carried no `expect` digest.
    Unverified,
}

impl Verdict {
    /// One-line banner text (`MATCH`, `DRIFT: a, b`, `DONE (no expected digest)`).
    #[must_use]
    pub fn banner(&self) -> String {
        match self {
            Self::Match => "MATCH".to_string(),
            Self::Drift(fields) => format!("DRIFT: {}", fields.join(", ")),
            Self::Unverified => "DONE (no expected digest)".to_string(),
        }
    }
}

/// A finished replay: verdict plus the digest actually reached.
#[derive(Debug, Clone)]
pub struct ReplayResult {
    pub verdict: Verdict,
    pub digest: Digest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlayerPhase {
    /// Restart requested; waiting for `SessionEpoch` to advance.
    AwaitingRestart { from_epoch: SessionEpoch },
    Running,
    Finished,
}

/// Active replay of a scenario.
#[derive(Debug, Clone)]
pub struct Player {
    pub scenario: Scenario,
    pub capture_mode: CaptureMode,
    phase: PlayerPhase,
    action_idx: usize,
    capture_idx: usize,
}

impl Player {
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.phase == PlayerPhase::Finished
    }
}

#[derive(Debug, Clone)]
struct RecordSetup {
    seed: u64,
    env: EnvironmentConfig,
    test_map: SiteTestMap,
    upgrades: UpgradeMultipliers,
}

/// Active recording.
#[derive(Debug, Clone)]
pub struct Recorder {
    from_epoch: SessionEpoch,
    setup: Option<RecordSetup>,
    controls: Vec<ControlEntry>,
    actions: Vec<ScenarioAction>,
    last: Option<DriveControl>,
}

impl Recorder {
    /// Ticks recorded so far are not stored here; read `SimTick`. This is the control count.
    #[must_use]
    pub fn control_changes(&self) -> usize {
        self.controls.len()
    }
}

/// What the replay layer is doing.
#[derive(Debug, Clone, Default)]
pub enum ReplayMode {
    #[default]
    Idle,
    Recording(Recorder),
    Replaying(Box<Player>),
}

/// Record/replay session state.
#[derive(Resource, Debug, Default)]
pub struct ReplaySession {
    pub mode: ReplayMode,
    /// Last finished recording (kept for `Replay Last`).
    pub last_recording: Option<Scenario>,
    /// Recording finished (F9 or an auto-stopping restart) but not yet written to disk; the
    /// presentation layer takes it and saves it.
    pub pending_save: Option<Scenario>,
    /// Result of the last finished replay.
    pub last_result: Option<ReplayResult>,
    /// Captures taken during the current/last replay, in order.
    pub captures: Vec<CaptureTelemetry>,
    /// Capture waiting for a screenshot (`CaptureMode::Screenshot`); the sim is frozen until
    /// the presentation layer calls [`complete_pending_capture`].
    pub pending_screenshot: Option<CaptureTelemetry>,
    /// Status line for HUD badges / CLI output.
    pub note: String,
}

impl ReplaySession {
    #[must_use]
    pub const fn is_recording(&self) -> bool {
        matches!(self.mode, ReplayMode::Recording(_))
    }

    #[must_use]
    pub const fn is_replaying(&self) -> bool {
        matches!(self.mode, ReplayMode::Replaying(_))
    }

    /// The finished replay's verdict, once judged.
    #[must_use]
    pub fn finished_verdict(&self) -> Option<&Verdict> {
        match &self.mode {
            ReplayMode::Replaying(p) if p.is_finished() => self.last_result.as_ref().map(|r| &r.verdict),
            _ => None,
        }
    }
}

/// Starts replaying `scenario`: applies its settings and seed, restarts the site and feeds the
/// recorded inputs from tick 0. Live drive/upgrade input is ignored until the replay is left.
pub fn begin_replay(world: &mut World, scenario: Scenario, capture_mode: CaptureMode) {
    let from_epoch = world.get_resource::<SessionEpoch>().copied().unwrap_or_default();
    if let Some(mut env) = world.get_resource_mut::<EnvironmentConfig>() {
        env.mode = scenario.settings.mode;
        env.level = scenario.settings.level;
        env.season = scenario.settings.season;
        env.weather = scenario.settings.weather;
    }
    world.insert_resource(scenario.settings.test_map);
    world.insert_resource(SiteSeedOverride(Some(scenario.seed)));
    world.insert_resource(InputAuthority::Replay);
    world.insert_resource(SimFreeze(false));
    if let Some(mut debug) = world.get_resource_mut::<DebugTelemetry>() {
        debug.debug_overlay_visible = scenario.overlays.telemetry || debug.debug_overlay_visible;
    }
    if let Some(mut next) = world.get_resource_mut::<NextState<AppState>>() {
        next.set(AppState::Playing);
    }
    world.send_event(SiteRestartRequested {
        preserve_layout: false,
    });
    let note = format!(
        "Replaying {} ticks of {:?} recorded on {}.",
        scenario.exit_tick, scenario.name, scenario.build
    );
    let mut session = world.get_resource_or_insert_with(ReplaySession::default);
    session.captures.clear();
    session.pending_screenshot = None;
    session.last_result = None;
    session.note = note;
    session.mode = ReplayMode::Replaying(Box::new(Player {
        scenario,
        capture_mode,
        phase: PlayerPhase::AwaitingRestart { from_epoch },
        action_idx: 0,
        capture_idx: 0,
    }));
}

/// Starts a recording: restarts the current site (like `Restart Site`) and captures every tick
/// from tick 0 until [`finish_recording`] or any other restart.
pub fn begin_recording(world: &mut World) {
    let from_epoch = world.get_resource::<SessionEpoch>().copied().unwrap_or_default();
    world.insert_resource(InputAuthority::Live);
    world.insert_resource(SimFreeze(false));
    if let Some(mut next) = world.get_resource_mut::<NextState<AppState>>() {
        next.set(AppState::Playing);
    }
    world.send_event(SiteRestartRequested {
        preserve_layout: true,
    });
    let mut session = world.get_resource_or_insert_with(ReplaySession::default);
    session.note = "Recording. F9 stops; any restart also stops.".to_string();
    session.mode = ReplayMode::Recording(Recorder {
        from_epoch,
        setup: None,
        controls: Vec::new(),
        actions: Vec::new(),
        last: None,
    });
}

/// Finishes the active recording, digests the current end state and returns the scenario
/// (also kept as `last_recording`). `None` if not recording or the site never started.
pub fn finish_recording(world: &mut World) -> Option<Scenario> {
    let recorder = {
        let mut session = world.get_resource_mut::<ReplaySession>()?;
        match std::mem::take(&mut session.mode) {
            ReplayMode::Recording(r) => r,
            other => {
                session.mode = other;
                return None;
            }
        }
    };
    let setup = recorder.setup?;
    let digest = compute_digest(world);
    let mut scenario = Scenario::new(
        "recording",
        setup.seed,
        ScenarioSettings {
            mode: setup.env.mode,
            level: setup.env.level,
            season: setup.env.season,
            weather: setup.env.weather,
            test_map: setup.test_map,
            upgrades: setup.upgrades,
        },
    );
    scenario.recorded_at = Some(utc_timestamp(std::time::SystemTime::now()));
    scenario.controls = recorder.controls;
    scenario.actions = recorder.actions;
    scenario.exit_tick = digest.ticks;
    scenario.expect = Some(digest);
    if let Some(mut session) = world.get_resource_mut::<ReplaySession>() {
        session.note = format!("Recorded {} ticks.", scenario.exit_tick);
        session.last_recording = Some(scenario.clone());
        session.pending_save = Some(scenario.clone());
    }
    Some(scenario)
}

/// Leaves a replay (or finished replay) and hands control back to live input.
pub fn exit_replay(world: &mut World) {
    if let Some(mut session) = world.get_resource_mut::<ReplaySession>()
        && session.is_replaying()
    {
        session.mode = ReplayMode::Idle;
        session.pending_screenshot = None;
        session.note = "Replay exited.".to_string();
    } else {
        return;
    }
    world.insert_resource(InputAuthority::Live);
    world.insert_resource(SimFreeze(false));
    world.insert_resource(SiteSeedOverride(None));
}

/// Called by the presentation layer once the pending screenshot is saved: records the capture
/// and releases the freeze (unless the replay just reached its exit tick).
pub fn complete_pending_capture(world: &mut World) {
    let Some(mut session) = world.get_resource_mut::<ReplaySession>() else {
        return;
    };
    if let Some(capture) = session.pending_screenshot.take() {
        session.captures.push(capture);
    }
}

/// Formats a `SystemTime` as `YYYY-MM-DDTHH:MM:SSZ` (UTC) without a date crate.
#[must_use]
pub fn utc_timestamp(time: std::time::SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

fn capture_telemetry(world: &mut World, spec: &CaptureSpec) -> CaptureTelemetry {
    let digest = compute_digest(world);
    CaptureTelemetry {
        label: spec.label.clone(),
        tick: digest.ticks,
        sim_secs: digest.elapsed,
        digest,
        debug: world.get_resource::<DebugTelemetry>().cloned().unwrap_or_default(),
        collapse_joins: world
            .get_resource::<CollapseTaskJoinStats>()
            .copied()
            .unwrap_or_default(),
    }
}

/// `Update`, after the restart chain: once the replay's (or recording's) own restart has been
/// handled, installs the recorded upgrades / captures the setup for tick 0.
pub fn arm_after_restart_system(world: &mut World) {
    let epoch = world.get_resource::<SessionEpoch>().copied().unwrap_or_default();
    let Some(mut session) = world.get_resource_mut::<ReplaySession>() else {
        return;
    };
    let mut install_upgrades = None;
    let mut want_setup = false;
    match &mut session.mode {
        ReplayMode::Replaying(player) => {
            if let PlayerPhase::AwaitingRestart { from_epoch } = player.phase
                && epoch != from_epoch
            {
                player.phase = PlayerPhase::Running;
                install_upgrades = Some(player.scenario.settings.upgrades.sanitized());
            }
        }
        ReplayMode::Recording(rec) => want_setup = rec.setup.is_none() && epoch != rec.from_epoch,
        ReplayMode::Idle => {}
    }
    if let Some(upgrades) = install_upgrades {
        world.insert_resource(upgrades);
        if let Some(mut progression) = world.get_resource_mut::<UpgradeProgression>() {
            progression.level_entry_upgrades = upgrades;
        }
    }
    if want_setup {
        let setup = RecordSetup {
            seed: world.get_resource::<SiteSeed>().map_or(0, |s| s.0),
            env: world.get_resource::<EnvironmentConfig>().cloned().unwrap_or_default(),
            test_map: world.get_resource::<SiteTestMap>().copied().unwrap_or_default(),
            upgrades: world.get_resource::<UpgradeMultipliers>().copied().unwrap_or_default(),
        };
        if let Some(mut session) = world.get_resource_mut::<ReplaySession>()
            && let ReplayMode::Recording(rec) = &mut session.mode
        {
            rec.setup = Some(setup);
        }
    }
}

/// `PdSystemSet::SimInput`, after the live latch: feeds the recorded control and due upgrade
/// picks for this tick (replay), or records the control the tick will consume (recording).
pub fn replay_sim_input_system(
    tick: Res<SimTick>,
    mut session: ResMut<ReplaySession>,
    mut track_input: ResMut<TrackInput>,
    mut picks: ResMut<UpgradePickQueue>,
) {
    match &mut session.mode {
        ReplayMode::Replaying(player) if player.phase == PlayerPhase::Running => {
            let control = player.scenario.control_at(tick.0);
            track_input.left_track = control.left;
            track_input.right_track = control.right;
            track_input.blade_pressed = control.blade;
            queue_due_actions(player, tick.0, &mut picks);
        }
        ReplayMode::Recording(rec) if rec.setup.is_some() => {
            let control = DriveControl {
                left: track_input.left_track,
                right: track_input.right_track,
                blade: track_input.blade_pressed,
            };
            if rec.last != Some(control) {
                rec.controls
                    .push(ControlEntry(tick.0, control.left, control.right, control.blade));
                rec.last = Some(control);
            }
        }
        _ => {}
    }
}

fn queue_due_actions(player: &mut Player, tick: u64, picks: &mut UpgradePickQueue) {
    while let Some(action) = player.scenario.actions.get(player.action_idx) {
        if action.at() > tick {
            break;
        }
        match action {
            ScenarioAction::Upgrade { choice, .. } => picks.push(*choice),
        }
        player.action_idx = player.action_idx.saturating_add(1);
    }
}

/// `RunFixedMainLoop::BeforeFixedMainLoop`: queues picks due before the next tick (covers a
/// held Campaign modal, where no tick runs), and ends a recording / leaves a replay when some
/// other restart is pending (the site is still intact, so the recording digests its end state).
pub fn replay_before_fixed_loop_system(world: &mut World) {
    let tick = world.get_resource::<SimTick>().map_or(0, |t| t.0);
    let restart_pending = world.get_resource::<RestartGate>().is_some_and(RestartGate::pending);
    let mut finish = false;
    let mut leave = false;
    let mut due = Vec::new();
    if let Some(mut session) = world.get_resource_mut::<ReplaySession>() {
        match &mut session.mode {
            ReplayMode::Recording(rec) => finish = restart_pending && rec.setup.is_some(),
            ReplayMode::Replaying(player) => {
                if restart_pending
                    && !matches!(player.phase, PlayerPhase::AwaitingRestart { .. })
                {
                    leave = true;
                } else if player.phase == PlayerPhase::Running {
                    let mut queue = UpgradePickQueue::default();
                    queue_due_actions(player, tick, &mut queue);
                    due = queue.picks;
                }
            }
            ReplayMode::Idle => {}
        }
    }
    if finish {
        let _ = finish_recording(world);
    }
    if leave {
        exit_replay(world);
    }
    if !due.is_empty()
        && let Some(mut picks) = world.get_resource_mut::<UpgradePickQueue>()
    {
        for choice in due {
            picks.push(choice);
        }
    }
}

/// `PdSystemSet::SimTickEnd`, after the tick counter advanced: freezes the simulation on a
/// capture tick or the exit tick so no further tick of this frame runs.
pub fn replay_tick_boundary_system(
    tick: Res<SimTick>,
    session: Res<ReplaySession>,
    mut freeze: ResMut<SimFreeze>,
) {
    if let ReplayMode::Replaying(player) = &session.mode
        && player.phase == PlayerPhase::Running
        && (tick.0 >= player.scenario.exit_tick
            || (player.capture_mode != CaptureMode::Ignore
                && player
                    .scenario
                    .captures
                    .get(player.capture_idx)
                    .is_some_and(|c| c.tick <= tick.0)))
    {
        freeze.0 = true;
    }
}

/// `Update` (after the physics step of the last tick): takes captures due at the current tick
/// and judges the end state at the exit tick. Releases the freeze for capture-only stops.
pub fn replay_boundary_update_system(world: &mut World) {
    let tick = world.get_resource::<SimTick>().map_or(0, |t| t.0);
    let (capture_due, at_exit, mode) = {
        let Some(session) = world.get_resource::<ReplaySession>() else {
            return;
        };
        if session.pending_screenshot.is_some() {
            return;
        }
        let ReplayMode::Replaying(player) = &session.mode else {
            return;
        };
        if player.phase != PlayerPhase::Running {
            return;
        }
        let capture = if player.capture_mode == CaptureMode::Ignore {
            None
        } else {
            player
                .scenario
                .captures
                .get(player.capture_idx)
                .filter(|c| c.tick <= tick)
                .cloned()
        };
        (capture, tick >= player.scenario.exit_tick, player.capture_mode)
    };

    if let Some(spec) = capture_due {
        let telemetry = capture_telemetry(world, &spec);
        let mut hold_here = false;
        if let Some(mut session) = world.get_resource_mut::<ReplaySession>() {
            if let ReplayMode::Replaying(player) = &mut session.mode {
                player.capture_idx = player.capture_idx.saturating_add(1);
            }
            if mode == CaptureMode::Screenshot {
                session.pending_screenshot = Some(telemetry);
                hold_here = true;
            } else {
                session.captures.push(telemetry);
                // More captures may share this tick; handle them on the following frames.
                hold_here = match &session.mode {
                    ReplayMode::Replaying(p) => {
                        p.scenario.captures.get(p.capture_idx).is_some_and(|c| c.tick <= tick)
                    }
                    _ => false,
                };
            }
        }
        if hold_here {
            // A tick-0 capture is taken outside any tick boundary, so freeze explicitly.
            world.insert_resource(SimFreeze(true));
            return;
        }
    }

    if at_exit {
        let digest = compute_digest(world);
        if let Some(mut session) = world.get_resource_mut::<ReplaySession>() {
            let verdict = match &session.mode {
                ReplayMode::Replaying(p) => match &p.scenario.expect {
                    Some(expected) => {
                        let drift = digest_drift(expected, &digest);
                        if drift.is_empty() { Verdict::Match } else { Verdict::Drift(drift) }
                    }
                    None => Verdict::Unverified,
                },
                _ => Verdict::Unverified,
            };
            session.note = format!("Replay finished at tick {tick}: {}", verdict.banner());
            session.last_result = Some(ReplayResult { verdict, digest });
            if let ReplayMode::Replaying(player) = &mut session.mode {
                player.phase = PlayerPhase::Finished;
            }
        }
        return;
    }

    if let Some(mut freeze) = world.get_resource_mut::<SimFreeze>() {
        freeze.0 = false;
    }
}

/// `Update`: appends applied upgrade picks to the active recording.
pub fn record_upgrade_actions_system(
    mut applied: EventReader<UpgradeApplied>,
    mut session: ResMut<ReplaySession>,
) {
    for ev in applied.read() {
        if let ReplayMode::Recording(rec) = &mut session.mode
            && rec.setup.is_some()
        {
            rec.actions.push(ScenarioAction::Upgrade {
                at: ev.at_tick,
                choice: ev.choice,
            });
        }
    }
}

/// Registers the record/replay systems around the simulation's tick boundaries.
pub struct PdReplayPlugin;

impl Plugin for PdReplayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ReplaySession>()
            .add_systems(
                RunFixedMainLoop,
                replay_before_fixed_loop_system
                    .in_set(bevy::app::RunFixedMainLoopSystem::BeforeFixedMainLoop)
                    .after(pd_core::gate_sim_on_pending_restart_system)
                    .before(pd_dozer::apply_upgrade_picks_system),
            )
            .add_systems(
                FixedUpdate,
                (
                    replay_sim_input_system
                        .in_set(PdSystemSet::SimInput)
                        .after(pd_dozer::latch_drive_control_system)
                        .before(pd_dozer::apply_upgrade_picks_system),
                    replay_tick_boundary_system
                        .in_set(PdSystemSet::SimTickEnd)
                        .after(pd_core::award_cash_and_advance_sim_tick_system),
                ),
            )
            .add_systems(
                Update,
                (
                    arm_after_restart_system,
                    record_upgrade_actions_system,
                    replay_boundary_update_system,
                )
                    .chain()
                    .after(pd_dozer::reset_dozer_on_site_restart_system)
                    .after(pd_destruction::ensure_site_buildings_spawned_system)
                    .before(PdSystemSet::WorldTilemapSync),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_timestamp_formats_known_instants() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_761_350_400);
        assert_eq!(utc_timestamp(t), "2025-10-25T00:00:00Z");
        assert_eq!(utc_timestamp(std::time::UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn verdict_banners() {
        assert_eq!(Verdict::Match.banner(), "MATCH");
        assert_eq!(Verdict::Drift(vec!["cash".into(), "dozer.x".into()]).banner(), "DRIFT: cash, dozer.x");
    }
}
