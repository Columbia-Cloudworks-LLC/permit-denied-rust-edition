//! Determinism gates for the deterministic simulation contract (CLAUDE.md Rule B):
//! same seed + same tick-indexed inputs => bit-identical simulation state, regardless of
//! session history (prior play, restarts, recycled entity indices, archetype creation order)
//! and of `HashMap` seeding, and with no ambiguous system ordering in `FixedUpdate`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use bevy::ecs::schedule::{LogLevel, ScheduleBuildSettings};
use bevy::prelude::*;
use pd_core::{
    AppState, CampaignLevel, CashLedger, ElapsedClock, EnvironmentConfig, GameMode,
    InputAuthority, SimOrdinal, SimTick, SiteRestartRequested, SiteSeedOverride, SplitMix64,
    WeatherSetting, WorldPose,
};
use pd_destruction::{BuildingGraphRegistry, CollapseTaskJoinStats, DebrisChunk, DestructibleProp};
use pd_dozer::{MechanicalStress, PlayerVehicle, TrackInput, UpgradeMultipliers};
use pd_world::PileField;

/// `(tick, left_track, right_track, blade_pressed)` change entries, held until the next one.
type Script = [(u64, f32, f32, bool)];

/// Pivot toward building #0, ram it blade-down, back off, then hit it again from the side.
const DEMOLITION_SCRIPT: &Script = &[
    (0, -1.0, 1.0, false),
    (48, 1.0, 1.0, false),
    (110, 1.0, 1.0, true),
    (300, -1.0, -1.0, false),
    (360, 1.0, -1.0, true),
    (380, 1.0, 1.0, true),
    (560, 0.0, 0.0, false),
];

/// Different play used only to dirty the session before the measured run.
const DIRT_SCRIPT: &Script = &[
    (0, 1.0, -1.0, true),
    (30, 1.0, 1.0, true),
    (200, -1.0, 1.0, false),
    (260, 1.0, 1.0, true),
];

fn set_control(app: &mut App, script: &Script, tick: u64) {
    let ctl = script
        .iter()
        .rev()
        .find(|c| c.0 <= tick)
        .copied()
        .unwrap_or((0, 0.0, 0.0, false));
    let mut input = app.world_mut().resource_mut::<TrackInput>();
    input.left_track = ctl.1;
    input.right_track = ctl.2;
    input.blade_pressed = ctl.3;
}

/// Restarts into `level`/`mode` with `seed`, then runs until tick 0 is about to execute.
fn start_site(app: &mut App, level: CampaignLevel, mode: GameMode, seed: u64) {
    {
        let mut env = app.world_mut().resource_mut::<EnvironmentConfig>();
        env.level = level;
        env.mode = mode;
        env.weather = WeatherSetting::On;
    }
    app.world_mut().resource_mut::<SiteSeedOverride>().0 = Some(seed);
    *app.world_mut().resource_mut::<InputAuthority>() = InputAuthority::Replay;
    app.world_mut()
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Playing);
    app.world_mut().send_event(SiteRestartRequested {
        preserve_layout: false,
    });
    // Frame 1 applies the transition and handles the restart while the gate holds the sim.
    app.update();
    assert_eq!(app.world().resource::<SimTick>().0, 0, "restart must hold tick 0");
}

/// Feeds `script` for `ticks` lockstep ticks, asserting exactly one tick per update.
fn run_script(app: &mut App, script: &Script, ticks: u64) {
    for _ in 0..ticks {
        let tick = app.world().resource::<SimTick>().0;
        set_control(app, script, tick);
        app.update();
        assert_eq!(app.world().resource::<SimTick>().0, tick + 1, "lockstep drifted");
    }
}

fn mix(acc: u64, value: u64) -> u64 {
    SplitMix64::mix(acc ^ value)
}

/// Bit-exact digest of the simulation, visited in stable (ordinal / node id) order.
fn snapshot(app: &mut App) -> Vec<(&'static str, u64)> {
    let mut out = Vec::new();
    let world = app.world_mut();
    out.push(("tick", world.resource::<SimTick>().0));
    out.push(("cash", u64::from(world.resource::<CashLedger>().current_cash)));
    out.push(("elapsed", u64::from(world.resource::<ElapsedClock>().elapsed_secs.to_bits())));
    let stress = *world.resource::<MechanicalStress>();
    out.push(("heat", u64::from(stress.engine_heat_pct.to_bits())));
    out.push(("track", u64::from(stress.track_stress_pct.to_bits())));
    let up = *world.resource::<UpgradeMultipliers>();
    out.push(("upgrades", mix(u64::from(up.ar400_edge.to_bits()), u64::from(up.aux_ram.to_bits()))));
    out.push(("pile_mass", u64::from(world.resource::<PileField>().total_mass().to_bits())));

    let mut dozer = world.query_filtered::<(&WorldPose, &PlayerVehicle), With<PlayerVehicle>>();
    let (pose, veh) = dozer.iter(world).next().map(|(p, v)| (*p, *v)).unwrap();
    out.push(("dozer.x", u64::from(pose.pos.x.to_bits())));
    out.push(("dozer.y", u64::from(pose.pos.y.to_bits())));
    out.push(("dozer.heading", u64::from(pose.heading_rad.to_bits())));
    out.push(("dozer.v", mix(u64::from(veh.vx.to_bits()), u64::from(veh.vy.to_bits()))));

    let registry = world.resource::<BuildingGraphRegistry>();
    let mut buildings: Vec<_> = registry.buildings.values().collect();
    buildings.sort_by_key(|b| b.site_ordinal);
    let (mut intact, mut cell_hash) = (0_u64, 0_u64);
    for b in buildings {
        let mut ids: Vec<_> = b.nodes.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let n = &b.nodes[&id];
            intact += u64::from(n.intact);
            cell_hash = mix(cell_hash, mix(u64::from(n.hp.to_bits()), n.damage_state as u64));
        }
    }
    out.push(("cells.intact", intact));
    out.push(("cells.hash", cell_hash));

    let mut debris = world.query::<(&SimOrdinal, &DebrisChunk, &WorldPose)>();
    let mut rows: Vec<_> = debris
        .iter(world)
        .map(|(o, c, p)| (o.0, c.tier as u64, p.pos.x.to_bits(), p.pos.y.to_bits(), c.mass.to_bits()))
        .collect();
    rows.sort_unstable_by_key(|r| r.0);
    out.push(("debris.count", rows.len() as u64));
    out.push((
        "debris.hash",
        rows.iter().fold(0, |acc, r| {
            mix(acc, mix(r.0, mix(r.1, mix(u64::from(r.2), mix(u64::from(r.3), u64::from(r.4))))))
        }),
    ));

    let mut props = world.query::<(&SimOrdinal, &DestructibleProp)>();
    let mut prop_rows: Vec<_> = props.iter(world).map(|(o, p)| (o.0, p.hp.to_bits())).collect();
    prop_rows.sort_unstable();
    out.push(("props.hash", prop_rows.iter().fold(0, |acc, r| mix(acc, mix(r.0, u64::from(r.1))))));

    let mut agents = world.query::<(&pd_ai::AgentOrdinal, &WorldPose)>();
    let mut agent_rows: Vec<_> = agents
        .iter(world)
        .map(|(o, p)| (o.0, p.pos.x.to_bits(), p.pos.y.to_bits()))
        .collect();
    agent_rows.sort_unstable();
    out.push((
        "agents.hash",
        agent_rows
            .iter()
            .fold(0, |acc, r| mix(acc, mix(u64::from(r.0), mix(u64::from(r.1), u64::from(r.2))))),
    ));
    out
}

fn drift(a: &[(&'static str, u64)], b: &[(&'static str, u64)]) -> Vec<&'static str> {
    a.iter().zip(b).filter(|(x, y)| x.1 != y.1).map(|(x, _)| x.0).collect()
}

fn measured_run(app: &mut App) -> Vec<(&'static str, u64)> {
    start_site(app, CampaignLevel::CityBorough5, GameMode::Sandbox, 0xD00D);
    run_script(app, DEMOLITION_SCRIPT, 640);
    snapshot(app)
}

/// Same seed + same inputs in two fresh apps (different `HashMap` seeds) => identical state.
/// Also guards that the script really exercises collapse BFS and debris.
#[test]
fn fresh_runs_are_bit_identical() {
    let mut a = permit_denied::headless_sim_app();
    let mut b = permit_denied::headless_sim_app();
    let sa = measured_run(&mut a);
    let sb = measured_run(&mut b);
    let get = |s: &[(&str, u64)], k: &str| s.iter().find(|r| r.0 == k).map_or(0, |r| r.1);
    assert!(get(&sa, "debris.count") > 0, "script must spawn debris: {sa:?}");
    assert!(get(&sa, "cash") > 0);
    assert!(a.world().resource::<CollapseTaskJoinStats>().joins > 0, "script must run collapse BFS");
    assert_eq!(drift(&sa, &sb), Vec::<&str>::new(), "fresh runs drifted");
}

/// Replay after a dirty session (prior demolition on other levels, restarts, recycled entity
/// indices, archetypes created in a different order) must match a fresh run bit-for-bit.
#[test]
fn dirty_history_run_matches_fresh_run() {
    let mut fresh = permit_denied::headless_sim_app();
    let expected = measured_run(&mut fresh);

    let mut dirty = permit_denied::headless_sim_app();
    start_site(&mut dirty, CampaignLevel::Village2, GameMode::Sandbox, 7);
    run_script(&mut dirty, DIRT_SCRIPT, 420);
    start_site(&mut dirty, CampaignLevel::CityDowntown6, GameMode::Sandbox, 99);
    run_script(&mut dirty, DEMOLITION_SCRIPT, 300);
    start_site(&mut dirty, CampaignLevel::CityBorough5, GameMode::Sandbox, 1);
    run_script(&mut dirty, DIRT_SCRIPT, 150);
    let actual = measured_run(&mut dirty);

    assert_eq!(drift(&expected, &actual), Vec::<&str>::new(), "dirty-history run drifted");
}

/// Rule B: no two `FixedUpdate` systems with conflicting access may run in an unspecified order.
#[test]
fn fixed_update_schedule_has_no_ambiguities() {
    let mut app = permit_denied::headless_sim_app();
    app.edit_schedule(FixedUpdate, |schedule| {
        schedule.set_build_settings(ScheduleBuildSettings {
            ambiguity_detection: LogLevel::Error,
            ..Default::default()
        });
    });
    start_site(&mut app, CampaignLevel::County1, GameMode::Sandbox, 1);
    run_script(&mut app, DEMOLITION_SCRIPT, 2);
}

/// Campaign run through the `$350` milestone: the modal pause lands on the same tick, and how
/// many frames the player idles in the modal (wall-clock pause length) cannot change the run.
fn campaign_run(modal_idle_frames: u32) -> (u64, Vec<(&'static str, u64)>) {
    let mut app = permit_denied::headless_sim_app();
    start_site(&mut app, CampaignLevel::CityBorough5, GameMode::Campaign, 0xD00D);
    let mut paused_at = None;
    let mut guard = 0_u32;
    while app.world().resource::<SimTick>().0 < 640 {
        guard += 1;
        assert!(guard < 5_000, "campaign run stalled");
        let tick = app.world().resource::<SimTick>().0;
        let pending = app.world().resource::<pd_dozer::UpgradeProgression>().pending_upgrade_choices;
        if pending > 0 {
            paused_at.get_or_insert(tick);
            for _ in 0..modal_idle_frames {
                app.update();
                assert_eq!(app.world().resource::<SimTick>().0, tick, "sim ticked in the modal");
            }
            app.world_mut()
                .resource_mut::<pd_dozer::UpgradePickQueue>()
                .push(pd_dozer::UpgradeChoice::Ar400Edge);
            app.update();
            continue;
        }
        set_control(&mut app, DEMOLITION_SCRIPT, tick);
        app.update();
    }
    (paused_at.unwrap_or(u64::MAX), snapshot(&mut app))
}

#[test]
fn campaign_milestone_pause_is_tick_exact_and_pause_length_independent() {
    let (tick_a, a) = campaign_run(1);
    let (tick_b, b) = campaign_run(37);
    assert_ne!(tick_a, u64::MAX, "script must cross the $350 milestone");
    assert_eq!(tick_a, tick_b, "milestone pause landed on different ticks");
    assert_eq!(drift(&a, &b), Vec::<&str>::new(), "modal pause length changed the run");
}
