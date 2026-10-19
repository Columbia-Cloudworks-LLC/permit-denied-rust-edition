//! `pd_dozer`: Armored bulldozer skid-steer physics, timed powered blade surge (`push_timer_secs`,
//! `push_cooldown_secs`), 5-point blade contact (`compute_blade_contact_points`), heavy machinery
//! momentum & pivot turn scrubbing, chassis pitch/roll spring-damper, `ENGINE HEAT` / `TRACK STRESS`
//! gauges, interactive milestone upgrades (`AR400 Edge`, `Charge-Air Kit`, `Aux Ram`), multi-part
//! 2.5D dozer sprite hierarchy, tread decals, and anti-wedging depenetration watchdog.
//!
//! Depends only on `pd_core`.

use avian2d::{prelude::*, prepare::PrepareConfig, sync::SyncConfig};
use bevy::prelude::*;
use pd_core::{
    AppState, AudioSfxKind, AudioTriggerEvent, CampaignLevel, CashLedger, CashPopupPool,
    DecalKind, DecalSpawnRequest, EnvironmentConfig, GameMode, ISO_BIAS_DOZER, IsoBoxAssembly,
    IsoExtrudedBox, IsoLayerBias, ParticleBurstEvent, ParticleBurstKind, PdSystemSet,
    PhysicsError, SessionEpoch, SiteRestartRequested, SiteScoped, WorldPose, heading_offset,
    iso_depth_key, sanitize_dt, sanitize_f32, sanitize_vec2, world_to_iso_screen,
};
use serde::{Deserialize, Serialize};

/// World-space pixels/units per simulation meter (`PhysicsPlugins::default().with_length_unit(16.0)`).
pub const WORLD_UNITS_PER_METER: f32 = 16.0;

/// Maximum allowed upgrade multiplier to prevent infinite velocity/impulse explosions.
pub const MAX_SAFE_MULTIPLIER: f32 = 25.0;

/// Maximum safe impulse applied to the dozer per fixed timestep.
pub const MAX_DOZER_IMPULSE_PER_TICK: f32 = 450_000.0;

/// Penetration threshold (meters) triggering the anti-wedging depenetration watchdog.
pub const MAX_SAFE_PENETRATION_DEPTH: f32 = 0.35;

/// Number of consecutive wedged fixed ticks before auto-crushing/depenetrating (+0.5m).
pub const WEDGED_TICK_THRESHOLD: u32 = 3;

/// Depenetration recovery step (meters) along contact normal.
pub const DEPENETRATION_STEP_METERS: f32 = 0.5;

/// Baseline forward top speed (`DOZER.maxSpeed = 8.6 m/s` * `16.0` world units/m = `137.6`).
pub const BASE_TOP_SPEED: f32 = 8.6 * WORLD_UNITS_PER_METER;

/// Baseline reverse top speed (`DOZER.maxReverse = 4.6 m/s` * `16.0` world units/m = `73.6`).
pub const BASE_MAX_REVERSE_SPEED: f32 = 4.6 * WORLD_UNITS_PER_METER;

/// Forward acceleration (`DOZER.accel = 14.8 m/s²` * `16.0`).
pub const DOZER_ACCEL: f32 = 14.8 * WORLD_UNITS_PER_METER;

/// Reverse acceleration (`DOZER.reverseAccel = 11.2 m/s²` * `16.0`).
pub const DOZER_REVERSE_ACCEL: f32 = 11.2 * WORLD_UNITS_PER_METER;

/// Coasting deceleration when throttle is released (`DOZER.coast = 3.4 m/s²` * `16.0`).
pub const DOZER_COAST_DECEL: f32 = 3.4 * WORLD_UNITS_PER_METER;

/// Lateral track grip decay rate (`DOZER.lateralGrip = 18.0 /s`).
pub const DOZER_LATERAL_GRIP: f32 = 18.0;

/// Baseline zero-turn yaw rate (`DOZER.steer = 2.85 rad/s`).
pub const BASE_YAW_RATE: f32 = 2.85;

/// Speed-dependent steering falloff (`DOZER.steerSpeedFalloff = 0.2` per m/s).
pub const DOZER_STEER_SPEED_FALLOFF: f32 = 0.2;

/// Fraction of forward speed lost during a full pivot turn (`DOZER.scrubSlow = 0.3`).
pub const DOZER_SCRUB_SLOW: f32 = 0.3;

/// Deceleration rate when pivoting scrubs excess speed (`DOZER.scrubDecel = 6.0 m/s²` * `16.0`).
pub const DOZER_SCRUB_DECEL: f32 = 6.0 * WORLD_UNITS_PER_METER;

/// Powered blade push forward acceleration surge (`DOZER.pushForce = 18.0 m/s²` * `16.0`).
pub const DOZER_PUSH_FORCE: f32 = 18.0 * WORLD_UNITS_PER_METER;

/// Baseline powered blade push duration (`DOZER.pushSeconds = 0.72 s`).
pub const DOZER_PUSH_SECONDS: f32 = 0.72;

/// Cooldown after a powered blade push completes (`DOZER.pushCooldown = 2.15 s`).
pub const DOZER_PUSH_COOLDOWN: f32 = 2.15;

/// Top speed multiplier while powered blade push is active (`1.12x` in `capDozerSpeed`).
pub const DOZER_PUSH_SPEED_CAP_MUL: f32 = 1.12;

/// Impact speed loss factor (`DOZER.impactLoss = 0.62`).
pub const DOZER_IMPACT_LOSS: f32 = 0.62;

/// Blade forward reach from dozer center with blade raised (`DOZER.bladeReach = 1.42 m` * `16.0`).
pub const DOZER_BLADE_REACH: f32 = 1.42 * WORLD_UNITS_PER_METER;

/// Extra forward blade extension when lowered (`+0.12 m` * `16.0` in `bladePoints`).
pub const DOZER_BLADE_DOWN_EXTEND: f32 = 0.12 * WORLD_UNITS_PER_METER;

/// Half-width of the front AR400 blade (`DOZER.bladeHalf = 1.08 m` * `16.0`).
pub const DOZER_BLADE_HALF: f32 = 1.08 * WORLD_UNITS_PER_METER;

/// Depth of the front AR400 blade (`DOZER.bladeDepth = 0.38 m` * `16.0`).
pub const DOZER_BLADE_DEPTH: f32 = 0.38 * WORLD_UNITS_PER_METER;

/// Chassis pitch constants (`PITCH` in `dozer.ts`).
pub const PITCH_BRAKE: f32 = 0.07;
pub const PITCH_SQUAT: f32 = -0.042;
pub const PITCH_IMPACT: f32 = 0.035;
pub const PITCH_MAX: f32 = 0.13;
pub const PITCH_BLADE_DROP: f32 = 0.3;

/// Chassis roll constants (`ROLL` in `dozer.ts`).
pub const ROLL_IMPACT: f32 = 0.03;
pub const ROLL_MAX: f32 = 0.026;

/// Chassis spring-damper constants (`CHASSIS` in `dozer.ts`).
pub const CHASSIS_SPRING_STIFFNESS: f32 = 121.0;
pub const CHASSIS_SPRING_DAMPING: f32 = 13.0;

/// Campaign cash milestone thresholds (`UPGRADE_MILESTONES = [350, 900, 1600]` in `constants.ts`).
pub const UPGRADE_MILESTONES: [u32; 3] = [350, 900, 1_600];

/// Step for additional cash milestones after the initial 3 campaign milestones.
pub const SUBSEQUENT_MILESTONE_STEP: u32 = 900;

/// Per-pick upgrade bonus increments (`UPGRADE_INCREMENTS` in `upgrades.ts`).
pub const UPGRADE_INCREMENT_AR400_EDGE: f32 = 0.42;
pub const UPGRADE_INCREMENT_CHARGE_AIR_KIT: f32 = 0.28;
pub const UPGRADE_INCREMENT_AUX_RAM: f32 = 0.35;

/// Marker component for the player's armored bulldozer entity, storing persistent velocity,
/// timed powered push state, chassis pitch/roll spring-damper state, and odometer distance.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct PlayerVehicle {
    pub heading_radians: f32,
    pub vx: f32,
    pub vy: f32,
    pub push_timer_secs: f32,
    pub push_cooldown_secs: f32,
    pub blade_landed_this_tick: bool,
    pub pitch: f32,
    pub pitch_vel: f32,
    pub roll: f32,
    pub roll_vel: f32,
    pub ride_roll: f32,
    pub last_impact: f32,
    pub odometer_dist: f32,
    pub blade_shear_multiplier: f32,
    pub pushing_force_multiplier: f32,
    pub blade_shear_carry: f32,
    pub distance_since_last_tread_decal: f32,
    pub exhaust_smoke_timer: f32,
}

impl PlayerVehicle {
    /// Computes the number of intact ground-story structural nodes sheared by a blade/hull impact
    /// while preserving fractional `AR400 Edge` (`+42%` per level) increments via `blade_shear_carry`.
    pub fn consume_blade_shear_nodes(
        &mut self,
        blade_state: BladeState,
        upgrades: &UpgradeMultipliers,
    ) -> usize {
        match blade_state {
            BladeState::BladeUp => 1,
            BladeState::BladeDown => {
                let fallback_mul = upgrades.sanitized().ar400_edge;
                let raw_mul = if self.blade_shear_multiplier > 1.0 {
                    self.blade_shear_multiplier.max(fallback_mul)
                } else {
                    fallback_mul
                };
                let shear_mul = sanitize_f32(raw_mul, 1.0).clamp(1.0, MAX_SAFE_MULTIPLIER);
                let carry = sanitize_f32(self.blade_shear_carry, 0.0).clamp(-0.5, 0.5);
                let exact_nodes = 2.0 * shear_mul + carry;
                let max_shear_nodes = 2.0 * MAX_SAFE_MULTIPLIER;
                let rounded = sanitize_f32(exact_nodes.round(), 2.0).clamp(2.0, max_shear_nodes);
                self.blade_shear_carry =
                    sanitize_f32(exact_nodes - rounded, 0.0).clamp(-0.5, 0.5);
                rounded as usize
            }
        }
    }
}

/// Marker component for the articulating hydraulic AR400 blade visual child sprite.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct DozerBladeVisual;

/// Hydraulic blade state machine (`BLADE UP` vs `BLADE DOWN`).
#[derive(Component, Resource, Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum BladeState {
    #[default]
    BladeUp,
    BladeDown,
}

impl BladeState {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::BladeUp => "BLADE UP",
            Self::BladeDown => "BLADE DOWN",
        }
    }

    #[must_use]
    pub const fn toggle(self) -> Self {
        match self {
            Self::BladeUp => Self::BladeDown,
            Self::BladeDown => Self::BladeUp,
        }
    }
}

/// Interactive equipment upgrade choices (`1` = `AR400 Edge`, `2` = `Charge-Air Kit`, `3` = `Aux Ram`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UpgradeChoice {
    /// `1`: Stronger Blade (`AR400 Edge`, `+42%` blade damage per level).
    Ar400Edge,
    /// `2`: More Engine (`Charge-Air Kit`, `+28%` engine performance per level).
    ChargeAirKit,
    /// `3`: Faster Powered Push (`Aux Ram`, `+35%` push duration/force per level).
    AuxRam,
}

impl UpgradeChoice {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ar400Edge => "1 · AR400 Edge (+42%)",
            Self::ChargeAirKit => "2 · Charge-Air Kit (+28%)",
            Self::AuxRam => "3 · Aux Ram (+35%)",
        }
    }
}

/// Live upgrade multipliers tracked on the cab dashboard HUD and incremented via player choices.
#[derive(Component, Resource, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct UpgradeMultipliers {
    /// Multiplies structural shear damage on blade contact (`+0.42x` per level).
    pub ar400_edge: f32,
    /// Increases horsepower, top speed, acceleration, and heat dissipation (`+0.28x` per level).
    pub charge_air_kit: f32,
    /// Increases powered blade push duration and hydraulic pushing force (`+0.35x` per level).
    pub aux_ram: f32,
}

impl Default for UpgradeMultipliers {
    fn default() -> Self {
        Self {
            ar400_edge: 1.0,
            charge_air_kit: 1.0,
            aux_ram: 1.0,
        }
    }
}

impl UpgradeMultipliers {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Sanitizes and clamps each multiplier to `[1.0, MAX_SAFE_MULTIPLIER]`.
    #[must_use]
    pub fn sanitized(&self) -> Self {
        Self {
            ar400_edge: sanitize_f32(self.ar400_edge, 1.0).clamp(1.0, MAX_SAFE_MULTIPLIER),
            charge_air_kit: sanitize_f32(self.charge_air_kit, 1.0).clamp(1.0, MAX_SAFE_MULTIPLIER),
            aux_ram: sanitize_f32(self.aux_ram, 1.0).clamp(1.0, MAX_SAFE_MULTIPLIER),
        }
    }

    /// Applies a single player-selected equipment upgrade increment (`+42%` blade, `+28%` engine,
    /// or `+35%` push), matching `upgradeModifiers` in `src/game/upgrades.ts`.
    pub fn apply_choice(&mut self, choice: UpgradeChoice) {
        *self = self.sanitized();
        match choice {
            UpgradeChoice::Ar400Edge => {
                self.ar400_edge = sanitize_f32(
                    self.ar400_edge + UPGRADE_INCREMENT_AR400_EDGE,
                    self.ar400_edge,
                )
                .clamp(1.0, MAX_SAFE_MULTIPLIER);
            }
            UpgradeChoice::ChargeAirKit => {
                self.charge_air_kit = sanitize_f32(
                    self.charge_air_kit + UPGRADE_INCREMENT_CHARGE_AIR_KIT,
                    self.charge_air_kit,
                )
                .clamp(1.0, MAX_SAFE_MULTIPLIER);
            }
            UpgradeChoice::AuxRam => {
                self.aux_ram =
                    sanitize_f32(self.aux_ram + UPGRADE_INCREMENT_AUX_RAM, self.aux_ram)
                        .clamp(1.0, MAX_SAFE_MULTIPLIER);
            }
        }
    }

    /// Computes modifiers from discrete upgrade level counts `(blade, engine, push)`.
    #[must_use]
    pub fn from_levels(blade: u32, engine: u32, push: u32) -> Self {
        Self {
            ar400_edge: 1.0 + (blade as f32) * UPGRADE_INCREMENT_AR400_EDGE,
            charge_air_kit: 1.0 + (engine as f32) * UPGRADE_INCREMENT_CHARGE_AIR_KIT,
            aux_ram: 1.0 + (push as f32) * UPGRADE_INCREMENT_AUX_RAM,
        }
        .sanitized()
    }
}

/// Resource tracking cash milestone thresholds, pending upgrade choices, and the level-entry
/// upgrade snapshot so retrying a Campaign level restores `level_entry_upgrades` while clearing
/// a level carries installed upgrades forward (`Issue #13`).
#[derive(Resource, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct UpgradeProgression {
    pub next_milestone_cash: u32,
    pub milestone_index: usize,
    pub pending_upgrade_choices: u8,
    pub level_entry_upgrades: UpgradeMultipliers,
    pub active_level: CampaignLevel,
}

impl Default for UpgradeProgression {
    fn default() -> Self {
        Self {
            next_milestone_cash: UPGRADE_MILESTONES[0],
            milestone_index: 0,
            pending_upgrade_choices: 0,
            level_entry_upgrades: UpgradeMultipliers::default(),
            active_level: CampaignLevel::County1,
        }
    }
}

impl UpgradeProgression {
    /// Checks `current_cash` against `next_milestone_cash` and increments `pending_upgrade_choices`
    /// for each crossed milestone (`$350`, `$900`, `$1,600`, ...). Returns newly unlocked choices.
    pub fn evaluate_cash_milestones(&mut self, current_cash: u32) -> u8 {
        let mut unlocked: u8 = 0;
        let mut guard: usize = 0;
        while current_cash >= self.next_milestone_cash && guard < 32 {
            guard = guard.saturating_add(1);
            unlocked = unlocked.saturating_add(1);
            self.pending_upgrade_choices = self.pending_upgrade_choices.saturating_add(1);
            self.milestone_index = self.milestone_index.saturating_add(1);
            self.next_milestone_cash = UPGRADE_MILESTONES
                .get(self.milestone_index)
                .copied()
                .unwrap_or_else(|| {
                    self.next_milestone_cash
                        .saturating_add(SUBSEQUENT_MILESTONE_STEP)
                });
        }
        unlocked
    }

    /// Attempts to apply an `UpgradeChoice`.
    /// - In `GameMode::Campaign`, requires `pending_upgrade_choices > 0` and consumes 1 choice.
    /// - In `GameMode::Sandbox`, always succeeds immediately without requiring a pending milestone.
    pub fn try_apply_choice(
        &mut self,
        upgrades: &mut UpgradeMultipliers,
        mode: GameMode,
        choice: UpgradeChoice,
    ) -> bool {
        match mode {
            GameMode::Campaign => {
                if self.pending_upgrade_choices == 0 {
                    return false;
                }
                self.pending_upgrade_choices = self.pending_upgrade_choices.saturating_sub(1);
                upgrades.apply_choice(choice);
                true
            }
            GameMode::Sandbox => {
                self.pending_upgrade_choices = self.pending_upgrade_choices.saturating_sub(1);
                upgrades.apply_choice(choice);
                true
            }
        }
    }

    /// Snapshots current `upgrades` as `level_entry_upgrades` when advancing to `next_level`.
    pub fn advance_level(
        &mut self,
        current_upgrades: &UpgradeMultipliers,
        next_level: CampaignLevel,
    ) {
        self.level_entry_upgrades = current_upgrades.sanitized();
        self.active_level = next_level;
        self.milestone_index = 0;
        self.next_milestone_cash = UPGRADE_MILESTONES[0];
        self.pending_upgrade_choices = 0;
    }

    /// Restores `upgrades` from `level_entry_upgrades` when retrying the current Campaign level.
    pub fn retry_level(&mut self, upgrades: &mut UpgradeMultipliers) {
        *upgrades = self.level_entry_upgrades.sanitized();
        self.milestone_index = 0;
        self.next_milestone_cash = UPGRADE_MILESTONES[0];
        self.pending_upgrade_choices = 0;
    }

    /// Full reset for starting a brand new game.
    pub fn reset_new_game(
        &mut self,
        upgrades: &mut UpgradeMultipliers,
        start_level: CampaignLevel,
    ) {
        upgrades.reset();
        *self = Self {
            active_level: start_level,
            ..Self::default()
        };
    }
}

/// Mechanical stress gauges (`ENGINE HEAT` and `TRACK STRESS` percentages) and anti-wedging counter.
#[derive(Component, Resource, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MechanicalStress {
    pub engine_heat_pct: f32,
    pub track_stress_pct: f32,
    pub overheated: bool,
    pub track_damaged: bool,
    pub sandbox_mode: bool,
    pub consecutive_wedged_ticks: u32,
}

impl Default for MechanicalStress {
    fn default() -> Self {
        Self {
            engine_heat_pct: 0.0,
            track_stress_pct: 0.0,
            overheated: false,
            track_damaged: false,
            sandbox_mode: false,
            consecutive_wedged_ticks: 0,
        }
    }
}

impl MechanicalStress {
    pub fn reset(&mut self, sandbox_mode: bool) {
        *self = Self {
            sandbox_mode,
            ..Self::default()
        };
    }
}

/// Differential track input, persistent velocity, timed powered blade state, chassis spring-damper
/// state, and contact telemetry fed into `compute_skid_steer_kinematics`.
#[derive(Component, Resource, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackInput {
    pub left_track: f32,
    pub right_track: f32,
    pub heading_radians: f32,
    pub external_load_factor: f32,
    pub penetration_depth: f32,
    pub contact_normal: Vec2,
    pub blade_pressed: bool,
    pub blade_down: bool,
    pub push_timer_secs: f32,
    pub push_cooldown_secs: f32,
    pub blade_landed_this_tick: bool,
    pub vx: f32,
    pub vy: f32,
    pub pitch: f32,
    pub pitch_vel: f32,
    pub roll: f32,
    pub roll_vel: f32,
    pub ride_roll: f32,
    pub odometer_dist: f32,
}

impl Default for TrackInput {
    fn default() -> Self {
        Self {
            left_track: 0.0,
            right_track: 0.0,
            heading_radians: 0.0,
            external_load_factor: 0.0,
            penetration_depth: 0.0,
            contact_normal: Vec2::Y,
            blade_pressed: false,
            blade_down: false,
            push_timer_secs: 0.0,
            push_cooldown_secs: 0.0,
            blade_landed_this_tick: false,
            vx: 0.0,
            vy: 0.0,
            pitch: 0.0,
            pitch_vel: 0.0,
            roll: 0.0,
            roll_vel: 0.0,
            ride_roll: 0.0,
            odometer_dist: 0.0,
        }
    }
}

impl TrackInput {
    /// Constructs a `TrackInput` from normalized `throttle` (`[-1.0, 1.0]`) and `steer` (`[-1.0, 1.0]`).
    #[must_use]
    pub fn from_throttle_steer(throttle: f32, steer: f32, blade_pressed: bool) -> Self {
        let t = sanitize_f32(throttle, 0.0).clamp(-1.0, 1.0);
        let s = sanitize_f32(steer, 0.0).clamp(-1.0, 1.0);
        Self {
            left_track: t - s,
            right_track: t + s,
            blade_pressed,
            ..Self::default()
        }
    }
}

/// Validated kinematic, powered blade, and chassis spring-damper output produced each fixed timestep.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KinematicOutput {
    pub linear_velocity: Vec2,
    pub angular_velocity: f32,
    pub blade_state: BladeState,
    pub push_timer_secs: f32,
    pub push_cooldown_secs: f32,
    pub blade_landed_this_tick: bool,
    pub pitch: f32,
    pub pitch_vel: f32,
    pub roll: f32,
    pub roll_vel: f32,
    pub odometer_dist: f32,
    pub blade_shear_multiplier: f32,
    pub pushing_force_multiplier: f32,
    pub depenetration_displacement: Option<Vec2>,
    pub crush_intersecting_block: bool,
}

#[inline]
fn approach_f32(current: f32, target: f32, step: f32) -> f32 {
    let s = sanitize_f32(step, 0.0).max(0.0);
    if current < target {
        (current + s).min(target)
    } else {
        (current - s).max(target)
    }
}

#[inline]
fn sign_i8(val: f32) -> i8 {
    if !val.is_finite() || val.abs() <= 1e-6 {
        0
    } else if val > 0.0 {
        1
    } else {
        -1
    }
}

/// Computes the 5 world-space blade contact points across the front AR400 blade width,
/// matching `bladePoints(d)` in `src/vehicle/dozer.ts` (`Issue #11`).
#[must_use]
pub fn compute_blade_contact_points(pos: Vec2, heading: f32, blade_down: bool) -> [Vec2; 5] {
    let safe_pos = sanitize_vec2(pos, Vec2::ZERO);
    let safe_heading = sanitize_f32(heading, 0.0);
    let (fy, fx) = safe_heading.sin_cos();
    let f = Vec2::new(fx, fy);
    let r = Vec2::new(-fy, fx);
    let reach = DOZER_BLADE_REACH
        + if blade_down {
            DOZER_BLADE_DOWN_EXTEND
        } else {
            0.0
        };
    let center = safe_pos + f * (reach + DOZER_BLADE_DEPTH * 0.2);

    let mut pts = [safe_pos; 5];
    for (idx, step) in (-2..=2).enumerate() {
        let t = (step as f32 * 0.5) * DOZER_BLADE_HALF;
        let pt = sanitize_vec2(center + r * t, safe_pos);
        if let Some(slot) = pts.get_mut(idx) {
            *slot = pt;
        }
    }
    pts
}

/// Applies an instantaneous velocity impulse to the chassis pitch and roll rates (`kickChassis` in `dozer.ts`).
pub fn kick_chassis(pitch_vel: &mut f32, roll_vel: &mut f32, pitch_kick: f32, roll_kick: f32) {
    let pv = sanitize_f32(*pitch_vel, 0.0) + sanitize_f32(pitch_kick, 0.0);
    let rv = sanitize_f32(*roll_vel, 0.0) + sanitize_f32(roll_kick, 0.0);
    *pitch_vel = sanitize_f32(pv, 0.0).clamp(-20.0, 20.0);
    *roll_vel = sanitize_f32(rv, 0.0).clamp(-20.0, 20.0);
}

/// Resolves a solid impact on the dozer along `contact_normal` (`(nx, ny)` pointing out of the
/// obstacle), updating position, velocity, `last_impact`, and rocking `pitch` and `roll`
/// (`applyDozerImpulse` in `src/vehicle/dozer.ts`). Returns impact speed in meters/second.
#[allow(clippy::too_many_arguments)]
pub fn apply_dozer_impulse(
    pos: &mut Vec2,
    velocity: &mut Vec2,
    heading: f32,
    pitch: &mut f32,
    roll: &mut f32,
    last_impact: &mut f32,
    contact_normal: Vec2,
    depth: f32,
    bounce: f32,
) -> f32 {
    let safe_n = sanitize_vec2(contact_normal, Vec2::ZERO).normalize_or_zero();
    if safe_n.length_squared() < 1e-6 {
        return 0.0;
    }
    let safe_depth = sanitize_f32(depth, 0.0).max(0.0);
    let safe_bounce = sanitize_f32(bounce, 0.12).clamp(0.0, 1.0);
    let safe_heading = sanitize_f32(heading, 0.0);

    *pos = sanitize_vec2(*pos + safe_n * safe_depth, *pos);
    let cur_v = sanitize_vec2(*velocity, Vec2::ZERO);
    let vn = cur_v.dot(safe_n);
    if vn >= 0.0 {
        return 0.0;
    }

    let impact_world = -vn;
    let impact_meters = impact_world / WORLD_UNITS_PER_METER;
    let loss_scale = (1.0 - DOZER_IMPACT_LOSS * 0.15).clamp(0.0, 1.0);
    let next_v = (cur_v - safe_n * (vn * (1.0 + safe_bounce))) * loss_scale;
    *velocity = sanitize_vec2(next_v, Vec2::ZERO);

    let cur_last = sanitize_f32(*last_impact, 0.0);
    *last_impact = sanitize_f32(cur_last.max((0.07 + impact_meters * 0.09).min(0.36)), 0.0);

    // Head-on hit throws the nose down (+pitch) and rear hit rocks it back; glancing hit lifts the struck side.
    let (fy, fx) = safe_heading.sin_cos();
    let cur_pitch = sanitize_f32(*pitch, 0.0);
    let cur_roll = sanitize_f32(*roll, 0.0);
    let next_pitch =
        cur_pitch - (safe_n.x * fx + safe_n.y * fy) * impact_meters * PITCH_IMPACT;
    let next_roll =
        cur_roll - (safe_n.y * fx - safe_n.x * fy) * impact_meters * ROLL_IMPACT;
    *pitch = sanitize_f32(next_pitch, cur_pitch).clamp(-PITCH_MAX, PITCH_MAX);
    *roll = sanitize_f32(next_roll, cur_roll).clamp(-ROLL_MAX, ROLL_MAX);

    sanitize_f32(impact_meters, 0.0)
}

/// Contract Function (Module B): Steps the timed powered blade push (`stepPush`), speed-falloff
/// steering, forward/reverse momentum & pivot turn scrubbing (`nextAlongSpeed`), lateral grip decay,
/// `1.12x` powered push speed cap (`capDozerSpeed`), chassis pitch/roll spring-damper (`settleChassis`),
/// `ENGINE HEAT` & `TRACK STRESS` gauges, and anti-wedging depenetration watchdog.
pub fn compute_skid_steer_kinematics(
    input: &TrackInput,
    blade_state: BladeState,
    upgrades: &UpgradeMultipliers,
    stress: &mut MechanicalStress,
    dt: f32,
) -> Result<KinematicOutput, PhysicsError> {
    // 1. Validate dt (clamp between 0.0001 and 0.05 seconds).
    let safe_dt = sanitize_dt(dt)?;

    if !input.left_track.is_finite()
        || !input.right_track.is_finite()
        || !input.heading_radians.is_finite()
    {
        return Err(PhysicsError::NonFiniteInput);
    }

    let raw_left = input.left_track.clamp(-2.0, 2.0);
    let raw_right = input.right_track.clamp(-2.0, 2.0);
    let throttle = ((raw_left + raw_right) * 0.5).clamp(-1.0, 1.0);
    let steer = ((raw_right - raw_left) * 0.5).clamp(-1.0, 1.0);
    let load = sanitize_f32(input.external_load_factor, 0.0).clamp(0.0, 5.0);

    // 2. Sanitize upgrade multipliers: clamp each multiplier to [1.0, MAX_SAFE_MULTIPLIER].
    let safe_upgrades = upgrades.sanitized();
    let blade_mul = safe_upgrades.ar400_edge;
    let engine_mul = safe_upgrades.charge_air_kit;
    let push_mul = safe_upgrades.aux_ram;

    // 3. Step Timed Powered Blade Push (`stepPush` in `src/vehicle/dozer.ts`).
    let mut push_t = sanitize_f32(input.push_timer_secs, 0.0).max(0.0);
    let mut push_cd = sanitize_f32(input.push_cooldown_secs, 0.0).max(0.0);
    let mut pitch = sanitize_f32(input.pitch, 0.0).clamp(-PITCH_MAX, PITCH_MAX);
    let mut pitch_vel = sanitize_f32(input.pitch_vel, 0.0).clamp(-20.0, 20.0);
    let mut roll = sanitize_f32(input.roll, 0.0).clamp(-ROLL_MAX, ROLL_MAX);
    let mut roll_vel = sanitize_f32(input.roll_vel, 0.0).clamp(-20.0, 20.0);
    let ride_roll = sanitize_f32(input.ride_roll, 0.0).clamp(-ROLL_MAX, ROLL_MAX);

    let want_blade = input.blade_pressed || blade_state == BladeState::BladeDown;
    if want_blade && push_cd <= 0.0 && push_t <= 0.0 {
        push_t = DOZER_PUSH_SECONDS * push_mul;
        push_cd = DOZER_PUSH_COOLDOWN;
    }
    let was_down = input.blade_down;
    let now_down = want_blade || push_t > 0.0;
    let blade_landed_this_tick = now_down && !was_down;
    if blade_landed_this_tick {
        pitch_vel = sanitize_f32(pitch_vel + PITCH_BLADE_DROP, pitch_vel).clamp(-20.0, 20.0);
    }

    let push_active_for_step = push_t > 0.0;
    if push_t > 0.0 {
        push_t = (push_t - safe_dt).max(0.0);
    }
    if push_cd > 0.0 && push_t <= 0.0 {
        push_cd = (push_cd - safe_dt).max(0.0);
    }

    let next_blade_state = if now_down {
        BladeState::BladeDown
    } else {
        BladeState::BladeUp
    };

    let (blade_shear_multiplier, blade_stress_factor) = match next_blade_state {
        BladeState::BladeUp => (1.0_f32, 0.6_f32),
        BladeState::BladeDown => (blade_mul, 1.45_f32),
    };

    // 4. Update Engine Heat & Track Stress percentages.
    let throttle_effort = throttle.abs();
    let turn_shear = steer.abs();

    let heat_gain = (throttle_effort * 6.5 + load * 14.0 * blade_stress_factor) * safe_dt;
    let heat_cool = (6.2 * engine_mul) * (1.15 - throttle_effort * 0.5) * safe_dt;
    let raw_heat = sanitize_f32(stress.engine_heat_pct, 0.0) + heat_gain - heat_cool;

    let stress_gain =
        (turn_shear * 11.0 * blade_stress_factor + load * 16.0 * blade_stress_factor) * safe_dt;
    let stress_cool = (9.0 * push_mul.sqrt()) * (1.0 - turn_shear * 0.5) * safe_dt;
    let raw_track_stress = sanitize_f32(stress.track_stress_pct, 0.0) + stress_gain - stress_cool;

    if stress.sandbox_mode {
        // Sandbox Mode rule: lock engine_heat and track_stress to safe bounds (no breakdowns).
        stress.engine_heat_pct = raw_heat.clamp(0.0, 75.0);
        stress.track_stress_pct = raw_track_stress.clamp(0.0, 75.0);
        stress.overheated = false;
        stress.track_damaged = false;
    } else {
        stress.engine_heat_pct = raw_heat.clamp(0.0, 100.0);
        stress.track_stress_pct = raw_track_stress.clamp(0.0, 100.0);
        if stress.engine_heat_pct >= 100.0 {
            stress.overheated = true;
        } else if stress.engine_heat_pct <= 60.0 {
            stress.overheated = false;
        }
        if stress.track_stress_pct >= 100.0 {
            stress.track_damaged = true;
        } else if stress.track_stress_pct <= 55.0 {
            stress.track_damaged = false;
        }
    }

    let breakdown_factor = if !stress.sandbox_mode && (stress.overheated || stress.track_damaged) {
        0.25_f32
    } else {
        1.0_f32
    };

    // 5. Speed-falloff steering, momentum (`nextAlongSpeed`), lateral grip decay, & turn scrub.
    let prev_vel = sanitize_vec2(Vec2::new(input.vx, input.vy), Vec2::ZERO);
    let speed_world = prev_vel.length();
    let speed_meters = speed_world / WORLD_UNITS_PER_METER;

    let (fy_prev, fx_prev) = input.heading_radians.sin_cos();
    let f_prev = Vec2::new(fx_prev, fy_prev);
    let along = prev_vel.dot(f_prev);

    let steer_scale = 1.0 / (1.0 + speed_meters * DOZER_STEER_SPEED_FALLOFF);
    let yaw = steer * BASE_YAW_RATE * steer_scale * breakdown_factor;
    let next_heading = sanitize_f32(input.heading_radians + yaw * safe_dt, input.heading_radians);
    let (fy, fx) = next_heading.sin_cos();
    let f = Vec2::new(fx, fy);

    let scrub = (yaw.abs() / BASE_YAW_RATE).clamp(0.0, 1.0);
    let push_surge = if push_active_for_step {
        DOZER_PUSH_FORCE * push_mul * safe_dt
    } else {
        0.0
    };

    let next_along = if throttle.abs() <= 1e-5 {
        approach_f32(
            along,
            0.0,
            (DOZER_COAST_DECEL + DOZER_SCRUB_DECEL * scrub) * safe_dt,
        ) + push_surge
    } else {
        let reach = 1.0 - DOZER_SCRUB_SLOW * scrub;
        let base_target = if throttle > 0.0 {
            BASE_TOP_SPEED * engine_mul * throttle.min(1.0)
        } else {
            BASE_MAX_REVERSE_SPEED * engine_mul * throttle.max(-1.0)
        };
        let target = base_target * reach * breakdown_factor;
        let scrubbing = scrub > 0.0
            && sign_i8(along) != 0
            && sign_i8(along) == sign_i8(target)
            && along.abs() > target.abs();
        let acc = if scrubbing {
            DOZER_SCRUB_DECEL
        } else if throttle > 0.0 {
            DOZER_ACCEL * engine_mul
        } else {
            DOZER_REVERSE_ACCEL * engine_mul
        };
        let stepped = approach_f32(along, target, acc * safe_dt);
        if push_active_for_step && throttle > 0.0 {
            stepped + push_surge
        } else {
            stepped
        }
    };

    let lat_vec = prev_vel - f * along;
    let lat_scale = (1.0 - DOZER_LATERAL_GRIP * safe_dt).max(0.0);
    let mut linear_velocity = f * next_along + lat_vec * lat_scale;

    // 6. Cap forward or reverse top speed (`capDozerSpeed` in `dozer.ts`) from resulting travel direction.
    let max_speed = if next_along < 0.0 {
        BASE_MAX_REVERSE_SPEED * engine_mul * breakdown_factor
    } else {
        BASE_TOP_SPEED
            * engine_mul
            * (if push_active_for_step {
                DOZER_PUSH_SPEED_CAP_MUL
            } else {
                1.0
            })
            * breakdown_factor
    };
    let new_speed = linear_velocity.length();
    if new_speed > max_speed && new_speed > 1e-5 {
        linear_velocity *= max_speed / new_speed;
    }

    let next_odo = sanitize_f32(
        input.odometer_dist + (next_along / WORLD_UNITS_PER_METER) * safe_dt,
        input.odometer_dist,
    );

    // 7. Chassis pitch & roll spring-damper (`settleChassis` in `dozer.ts`).
    let surge = ((next_along - along) / safe_dt / DOZER_ACCEL).clamp(-1.0, 1.0);
    let slowing = sign_i8(along) != 0 && sign_i8(surge) == -sign_i8(along);
    let pitch_target = surge * if slowing { -PITCH_BRAKE } else { PITCH_SQUAT };
    pitch_vel = sanitize_f32(
        pitch_vel
            + (CHASSIS_SPRING_STIFFNESS * (pitch_target - pitch)
                - CHASSIS_SPRING_DAMPING * pitch_vel)
                * safe_dt,
        0.0,
    )
    .clamp(-20.0, 20.0);
    pitch = sanitize_f32(pitch + pitch_vel * safe_dt, 0.0).clamp(-PITCH_MAX, PITCH_MAX);

    let steer_roll = (yaw / BASE_YAW_RATE).clamp(-1.0, 1.0) * ROLL_MAX;
    let roll_target = sanitize_f32(ride_roll + steer_roll, 0.0).clamp(-ROLL_MAX, ROLL_MAX);
    roll_vel = sanitize_f32(
        roll_vel
            + (CHASSIS_SPRING_STIFFNESS * (roll_target - roll) - CHASSIS_SPRING_DAMPING * roll_vel)
                * safe_dt,
        0.0,
    )
    .clamp(-20.0, 20.0);
    roll = sanitize_f32(roll + roll_vel * safe_dt, 0.0).clamp(-ROLL_MAX, ROLL_MAX);

    // 8. Anti-wedging & depenetration watchdog.
    let pen_depth = sanitize_f32(input.penetration_depth, 0.0);
    let mut depenetration_displacement = None;
    let mut crush_intersecting_block = false;

    if pen_depth > MAX_SAFE_PENETRATION_DEPTH {
        stress.consecutive_wedged_ticks = stress.consecutive_wedged_ticks.saturating_add(1);
        if stress.consecutive_wedged_ticks > WEDGED_TICK_THRESHOLD {
            let safe_normal = sanitize_vec2(input.contact_normal, Vec2::Y).normalize_or_zero();
            let recovery_dir = if safe_normal.length_squared() > 1e-4 {
                safe_normal
            } else {
                Vec2::Y
            };
            depenetration_displacement = Some(recovery_dir * DEPENETRATION_STEP_METERS);
            crush_intersecting_block = true;
            stress.consecutive_wedged_ticks = 0;
        }
    } else {
        stress.consecutive_wedged_ticks = 0;
    }

    // 9. Verify output linear and angular velocity vectors are finite before returning.
    let angular_velocity = yaw;
    if !linear_velocity.is_finite() || !angular_velocity.is_finite() {
        return Err(PhysicsError::NonFiniteOutput);
    }

    Ok(KinematicOutput {
        linear_velocity,
        angular_velocity,
        blade_state: next_blade_state,
        push_timer_secs: push_t,
        push_cooldown_secs: push_cd,
        blade_landed_this_tick,
        pitch,
        pitch_vel,
        roll,
        roll_vel,
        odometer_dist: next_odo,
        blade_shear_multiplier,
        pushing_force_multiplier: push_mul,
        depenetration_displacement,
        crush_intersecting_block,
    })
}

/// Builds the 2.5D multi-box assembly for the player's armored bulldozer, including
/// left/right tracks, grouser pads, front/rear idlers, armored hull belly, gold engine hood,
/// rear radiator grill, exhaust stack & cap, rear ripper assembly, armored operator cab,
/// tinted glass visor, gold cab roof, dual roof work lights, hydraulic push arms, and
/// 5-segment curved AR400 blade + top lip (`Issue #10`, matching `drawDozer` in `vehicles.ts`).
#[must_use]
pub fn build_dozer_iso_assembly(blade_state: BladeState) -> (IsoExtrudedBox, IsoBoxAssembly) {
    let base_hull = IsoExtrudedBox::new(38.0, 26.0, 7.0, Color::srgb(0.42, 0.31, 0.07))
        .with_offset(Vec2::ZERO, 2.0)
        .with_shadow(true);

    let mut parts = Vec::with_capacity(32);

    // 1. Left & right track assemblies + grouser pads + front/rear idlers
    for side in [-1.0_f32, 1.0_f32] {
        let track_across = side * 14.5;
        parts.push(
            IsoExtrudedBox::new(40.0, 8.0, 9.5, Color::srgb(0.16, 0.16, 0.17))
                .with_offset(Vec2::new(0.0, track_across), 0.0)
                .with_shadow(false),
        );
        parts.push(
            IsoExtrudedBox::new(30.0, 5.2, 5.0, Color::srgb(0.75, 0.56, 0.13))
                .with_offset(Vec2::new(0.0, track_across), 2.2)
                .with_shadow(false),
        );
        // Front & rear sprocket/idler hubs
        for end in [-1.0_f32, 1.0_f32] {
            parts.push(
                IsoExtrudedBox::new(6.8, 6.2, 6.8, Color::srgb(0.29, 0.25, 0.19))
                    .with_offset(Vec2::new(end * 15.0, track_across), 1.4)
                    .with_shadow(false),
            );
        }
        // Grouser bars along top of track
        for g in -3..=3 {
            parts.push(
                IsoExtrudedBox::new(1.8, 7.8, 0.9, Color::srgb(0.24, 0.25, 0.27))
                    .with_offset(Vec2::new(g as f32 * 5.2, track_across), 9.4)
                    .with_shadow(false),
            );
        }
    }

    // 2. Main gold engine hood & forward nose
    parts.push(
        IsoExtrudedBox::new(23.0, 21.0, 12.0, Color::srgb(0.85, 0.65, 0.16))
            .with_offset(Vec2::new(7.5, 0.0), 8.5)
            .with_shadow(false),
    );
    // Side engine vents
    parts.push(
        IsoExtrudedBox::new(14.0, 22.0, 5.5, Color::srgb(0.23, 0.19, 0.13))
            .with_offset(Vec2::new(9.0, 0.0), 11.5)
            .with_shadow(false),
    );
    // Front radiator grille
    parts.push(
        IsoExtrudedBox::new(3.0, 16.0, 9.5, Color::srgb(0.16, 0.14, 0.12))
            .with_offset(Vec2::new(18.5, 0.0), 9.5)
            .with_shadow(false),
    );

    // 3. Exhaust stack & rain cap
    parts.push(
        IsoExtrudedBox::new(2.8, 2.8, 12.0, Color::srgb(0.14, 0.14, 0.16))
            .with_offset(Vec2::new(9.0, -7.2), 20.0)
            .with_shadow(false),
    );
    parts.push(
        IsoExtrudedBox::new(3.8, 3.8, 1.5, Color::srgb(0.23, 0.23, 0.25))
            .with_offset(Vec2::new(9.0, -7.2), 31.8)
            .with_shadow(false),
    );

    // 4. Rear counterweight & 3-shank ripper
    parts.push(
        IsoExtrudedBox::new(10.0, 24.0, 11.0, Color::srgb(0.69, 0.51, 0.12))
            .with_offset(Vec2::new(-14.5, 0.0), 8.5)
            .with_shadow(false),
    );
    parts.push(
        IsoExtrudedBox::new(5.5, 19.0, 4.2, Color::srgb(0.23, 0.21, 0.18))
            .with_offset(Vec2::new(-21.5, 0.0), 3.0)
            .with_shadow(false),
    );
    for shank in [-1.0_f32, 0.0_f32, 1.0_f32] {
        parts.push(
            IsoExtrudedBox::new(6.0, 2.2, 6.8, Color::srgb(0.35, 0.33, 0.29))
                .with_offset(Vec2::new(-23.5, shank * 6.5), 0.5)
                .with_shadow(false),
        );
    }

    // 5. Armored ROPS Operator Cab, tinted glass, roof cap, and work lights
    parts.push(
        IsoExtrudedBox::new(15.0, 19.0, 4.2, Color::srgb(0.75, 0.56, 0.13))
            .with_offset(Vec2::new(-6.0, 0.0), 17.5)
            .with_shadow(false),
    );
    parts.push(
        IsoExtrudedBox::new(13.0, 17.0, 9.8, Color::srgb(0.22, 0.32, 0.37))
            .with_offset(Vec2::new(-6.0, 0.0), 21.5)
            .with_shadow(false),
    );
    parts.push(
        IsoExtrudedBox::new(16.0, 20.0, 2.5, Color::srgb(0.91, 0.72, 0.23))
            .with_offset(Vec2::new(-6.0, 0.0), 31.0)
            .with_shadow(false),
    );
    for side in [-1.0_f32, 1.0_f32] {
        parts.push(
            IsoExtrudedBox::new(2.2, 3.0, 1.8, Color::srgb(1.0, 0.96, 0.78))
                .with_offset(Vec2::new(1.2, side * 6.5), 32.0)
                .with_shadow(false),
        );
    }

    // 6. Hydraulic push arms & 5-segment curved AR400 Bulldozer Blade
    let (blade_z, blade_reach, blade_face_col, blade_lip_col) = match blade_state {
        BladeState::BladeUp => (
            6.5_f32,
            24.5_f32,
            Color::srgb(0.54, 0.56, 0.60),
            Color::srgb(0.91, 0.72, 0.23),
        ),
        BladeState::BladeDown => (
            0.5_f32,
            26.5_f32,
            Color::srgb(0.64, 0.66, 0.70),
            Color::srgb(0.98, 0.78, 0.24),
        ),
    };

    for side in [-1.0_f32, 1.0_f32] {
        parts.push(
            IsoExtrudedBox::new(20.0, 3.0, 3.2, Color::srgb(0.54, 0.41, 0.09))
                .with_offset(Vec2::new(13.0, side * 12.2), blade_z + 2.5)
                .with_shadow(false),
        );
    }

    // 5-segment curved AR400 blade
    let blade_width = 38.0_f32;
    let seg_w = blade_width / 5.0 + 0.8;
    for s in -2..=2 {
        let frac = s as f32 / 2.0;
        let curve_forward = (1.0 - frac * frac) * -2.8;
        let seg_across = frac * (blade_width * 0.41);
        parts.push(
            IsoExtrudedBox::new(6.2, seg_w, 13.5, blade_face_col)
                .with_offset(Vec2::new(blade_reach + curve_forward, seg_across), blade_z)
                .with_shadow(false),
        );
    }
    // Top spill guard lip
    parts.push(
        IsoExtrudedBox::new(4.2, blade_width + 1.0, 2.2, blade_lip_col)
            .with_offset(Vec2::new(blade_reach - 1.5, 0.0), blade_z + 13.2)
            .with_shadow(false),
    );
    // Hardened AR400 cutting edge at bottom
    parts.push(
        IsoExtrudedBox::new(5.5, blade_width + 1.0, 2.0, Color::srgb(0.24, 0.24, 0.26))
            .with_offset(Vec2::new(blade_reach + 1.2, 0.0), blade_z)
            .with_shadow(false),
    );

    (
        base_hull,
        IsoBoxAssembly {
            parts,
            alpha_multiplier: 1.0,
        },
    )
}

/// Spawns the player's armored bulldozer with a detailed multi-part 2.5D isometric box assembly
/// (left/right armored tracks, industrial gold chassis, armored cab, and hydraulic AR400 blade).
pub fn ensure_player_dozer_spawned_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    blade_state: Res<BladeState>,
    existing_dozer: Query<Entity, With<PlayerVehicle>>,
) {
    if existing_dozer.iter().next().is_some() {
        return;
    }

    let spawn_world_xy = Vec2::new(0.0, -60.0);
    let spawn_iso = world_to_iso_screen(spawn_world_xy, 0.0);
    let spawn_z = iso_depth_key(spawn_world_xy, 0.0, ISO_BIAS_DOZER);
    let (base_hull, assembly) = build_dozer_iso_assembly(*blade_state);

    commands
        .spawn((
            Name::new("Armored Dozer (PlayerVehicle)"),
            PlayerVehicle::default(),
            SiteScoped { epoch: *epoch },
            WorldPose::new(spawn_world_xy, 0.0, 0.0),
            IsoLayerBias(ISO_BIAS_DOZER),
            base_hull,
            assembly,
            Transform::from_xyz(spawn_iso.x, spawn_iso.y, spawn_z),
            GlobalTransform::default(),
            Visibility::default(),
            (
                RigidBody::Dynamic,
                Collider::rectangle(46.0, 30.0),
                Position(spawn_world_xy),
                Rotation::radians(0.0),
                LinearVelocity::default(),
                AngularVelocity::default(),
            ),
        ))
        .with_children(|parent| {
            // Articulating Hydraulic AR400 Edge Blade marker child for queries
            parent.spawn((
                Name::new("AR400 Hydraulic Dozer Blade"),
                DozerBladeVisual,
                Sprite {
                    color: Color::NONE,
                    custom_size: Some(Vec2::new(10.0, 38.0)),
                    ..Default::default()
                },
                Transform::from_xyz(25.0, 0.0, 0.8),
                GlobalTransform::default(),
                Visibility::Hidden,
            ));
        });
}

/// Reads keyboard input to update `TrackInput` (throttle, steering, and `Space` powered blade hold).
pub fn read_dozer_keyboard_input_system(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut track_input: ResMut<TrackInput>,
    mut audio_events: EventWriter<AudioTriggerEvent>,
) {
    let Some(keys) = keys else {
        return;
    };

    let blade_just_pressed = keys.just_pressed(KeyCode::Space)
        || keys.just_pressed(KeyCode::KeyF)
        || keys.just_pressed(KeyCode::ShiftLeft);
    if blade_just_pressed && track_input.push_cooldown_secs <= 0.0 && track_input.push_timer_secs <= 0.0 {
        audio_events.send(AudioTriggerEvent {
            kind: AudioSfxKind::BladeHydraulicHiss,
        });
    }

    track_input.blade_pressed = keys.pressed(KeyCode::Space)
        || keys.pressed(KeyCode::KeyF)
        || keys.pressed(KeyCode::ShiftLeft);

    let mut forward = 0.0_f32;
    let mut turn = 0.0_f32;

    if keys.pressed(KeyCode::KeyW) || keys.pressed(KeyCode::ArrowUp) {
        forward += 1.0;
    }
    if keys.pressed(KeyCode::KeyS) || keys.pressed(KeyCode::ArrowDown) {
        forward -= 1.0;
    }
    if keys.pressed(KeyCode::KeyA) || keys.pressed(KeyCode::ArrowLeft) {
        turn += 1.0;
    }
    if keys.pressed(KeyCode::KeyD) || keys.pressed(KeyCode::ArrowRight) {
        turn -= 1.0;
    }

    // Preserve full throttle + steer when driving and turning simultaneously
    let mut left = (forward - turn).clamp(-2.0, 2.0);
    let mut right = (forward + turn).clamp(-2.0, 2.0);

    // Direct independent track overrides (Q/Z left track, E/C right track)
    if keys.pressed(KeyCode::KeyQ) {
        left = 1.0;
    } else if keys.pressed(KeyCode::KeyZ) {
        left = -1.0;
    }
    if keys.pressed(KeyCode::KeyE) {
        right = 1.0;
    } else if keys.pressed(KeyCode::KeyC) {
        right = -1.0;
    }

    track_input.left_track = left;
    track_input.right_track = right;
}

/// Evaluates demolition cash milestones (`$350`, `$900`, `$1,600`) and handles interactive
/// `1` / `2` / `3` upgrade selection (`Issue #13`).
/// - In `GameMode::Campaign`, crossing a cash milestone pauses the simulation (`AppState::Paused`)
///   until the player selects an upgrade (`Digit1`, `Digit2`, `Digit3`, or HUD modal button).
/// - In `GameMode::Sandbox`, `Digit1`, `Digit2`, and `Digit3` apply upgrades immediately without pausing.
pub fn evaluate_upgrade_milestones_and_hotkeys_system(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    app_state: Res<State<AppState>>,
    mut next_state: ResMut<NextState<AppState>>,
    env_config: Res<EnvironmentConfig>,
    ledger: Res<CashLedger>,
    mut upgrades: ResMut<UpgradeMultipliers>,
    mut progression: ResMut<UpgradeProgression>,
    mut audio_events: EventWriter<AudioTriggerEvent>,
) {
    let _unlocked = progression.evaluate_cash_milestones(ledger.current_cash);
    let state = *app_state.get();

    let mut choice_applied = false;
    if (state == AppState::Playing || state == AppState::Paused)
        && let Some(keys) = keys
    {
        let maybe_choice =
            if keys.just_pressed(KeyCode::Digit1) || keys.just_pressed(KeyCode::Numpad1) {
                Some(UpgradeChoice::Ar400Edge)
            } else if keys.just_pressed(KeyCode::Digit2) || keys.just_pressed(KeyCode::Numpad2) {
                Some(UpgradeChoice::ChargeAirKit)
            } else if keys.just_pressed(KeyCode::Digit3) || keys.just_pressed(KeyCode::Numpad3) {
                Some(UpgradeChoice::AuxRam)
            } else {
                None
            };

        if let Some(choice) = maybe_choice
            && progression.try_apply_choice(&mut upgrades, env_config.mode, choice)
        {
            choice_applied = true;
            audio_events.send(AudioTriggerEvent {
                kind: AudioSfxKind::CashDing,
            });
        }
    }

    if env_config.mode == GameMode::Campaign {
        if progression.pending_upgrade_choices > 0 && state == AppState::Playing {
            next_state.set(AppState::Paused);
        } else if choice_applied
            && progression.pending_upgrade_choices == 0
            && state == AppState::Paused
        {
            next_state.set(AppState::Playing);
        }
    }
}

/// Executes fixed-timestep skid-steer kinematics in Cartesian `WorldPose` coordinates,
/// synchronizes solver corrections with Cartesian `Position`/`Rotation`, updates dozer velocity,
/// timed powered blade state, chassis pitch/roll spring-damper, and emits persistent tread track
/// decals + diesel smoke.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn fixed_dozer_kinematics_system(
    time: Res<Time>,
    env_config: Res<EnvironmentConfig>,
    mut popup_pool: ResMut<CashPopupPool>,
    mut track_input: ResMut<TrackInput>,
    mut blade_state: ResMut<BladeState>,
    upgrades: Res<UpgradeMultipliers>,
    mut stress: ResMut<MechanicalStress>,
    mut decal_writer: EventWriter<DecalSpawnRequest>,
    mut particle_writer: EventWriter<ParticleBurstEvent>,
    mut dozer_query: Query<(
        &mut PlayerVehicle,
        &mut WorldPose,
        &mut IsoExtrudedBox,
        &mut IsoBoxAssembly,
        Option<&mut Position>,
        Option<&mut Rotation>,
        &mut LinearVelocity,
        &mut AngularVelocity,
    )>,
    mut blade_visual_query: Query<
        (&mut Transform, &mut Sprite),
        (With<DozerBladeVisual>, Without<PlayerVehicle>),
    >,
) {
    stress.sandbox_mode = env_config.mode == GameMode::Sandbox;

    let Ok((
        mut vehicle,
        mut pose,
        mut base_box,
        mut assembly,
        mut phys_pos,
        mut phys_rot,
        mut lin_vel,
        mut ang_vel,
    )) = dozer_query.get_single_mut()
    else {
        return;
    };

    // 1. Feed any Cartesian physics solver corrections back into WorldPose before stepping
    if let Some(pos_comp) = phys_pos.as_deref() {
        pose.pos = sanitize_vec2(pos_comp.0, pose.pos);
    }
    if let Some(rot_comp) = phys_rot.as_deref() {
        let solved_heading = sanitize_f32(rot_comp.as_radians(), vehicle.heading_radians);
        vehicle.heading_radians = solved_heading;
        pose.heading_rad = solved_heading;
    }

    // Synchronize persistent vehicle state into TrackInput
    track_input.heading_radians = vehicle.heading_radians;
    track_input.vx = vehicle.vx;
    track_input.vy = vehicle.vy;
    track_input.push_timer_secs = vehicle.push_timer_secs;
    track_input.push_cooldown_secs = vehicle.push_cooldown_secs;
    track_input.pitch = vehicle.pitch;
    track_input.pitch_vel = vehicle.pitch_vel;
    track_input.roll = vehicle.roll;
    track_input.roll_vel = vehicle.roll_vel;
    track_input.ride_roll = vehicle.ride_roll;
    track_input.odometer_dist = vehicle.odometer_dist;

    let requested_blade = if track_input.blade_pressed {
        BladeState::BladeDown
    } else if !track_input.blade_down && *blade_state == BladeState::BladeDown {
        // Honor external test or UI state overrides when blade was not previously down
        BladeState::BladeDown
    } else {
        BladeState::BladeUp
    };

    let Ok(output) = compute_skid_steer_kinematics(
        &track_input,
        requested_blade,
        &upgrades,
        &mut stress,
        time.delta_secs(),
    ) else {
        lin_vel.0 = Vec2::ZERO;
        ang_vel.0 = 0.0;
        return;
    };

    // Persist kinematic & spring-damper outputs on PlayerVehicle and TrackInput
    vehicle.vx = output.linear_velocity.x;
    vehicle.vy = output.linear_velocity.y;
    vehicle.push_timer_secs = output.push_timer_secs;
    vehicle.push_cooldown_secs = output.push_cooldown_secs;
    vehicle.blade_landed_this_tick = output.blade_landed_this_tick;
    vehicle.pitch = output.pitch;
    vehicle.pitch_vel = output.pitch_vel;
    vehicle.roll = output.roll;
    vehicle.roll_vel = output.roll_vel;
    vehicle.odometer_dist = output.odometer_dist;
    vehicle.blade_shear_multiplier = output.blade_shear_multiplier;
    vehicle.pushing_force_multiplier = output.pushing_force_multiplier;

    track_input.vx = output.linear_velocity.x;
    track_input.vy = output.linear_velocity.y;
    track_input.blade_down = output.blade_state == BladeState::BladeDown;
    track_input.push_timer_secs = output.push_timer_secs;
    track_input.push_cooldown_secs = output.push_cooldown_secs;
    track_input.blade_landed_this_tick = output.blade_landed_this_tick;
    track_input.pitch = output.pitch;
    track_input.pitch_vel = output.pitch_vel;
    track_input.roll = output.roll;
    track_input.roll_vel = output.roll_vel;
    track_input.odometer_dist = output.odometer_dist;

    if *blade_state != output.blade_state {
        *blade_state = output.blade_state;
    }

    if let Ok(safe_dt) = sanitize_dt(time.delta_secs()) {
        vehicle.last_impact = (vehicle.last_impact - safe_dt).max(0.0);
        vehicle.heading_radians = sanitize_f32(
            vehicle.heading_radians + output.angular_velocity * safe_dt,
            vehicle.heading_radians,
        );
        pose.heading_rad = vehicle.heading_radians;
        pose.pos = sanitize_vec2(pose.pos + output.linear_velocity * safe_dt, pose.pos);
        pose.pitch_rad = output.pitch;
        pose.roll_rad = output.roll;

        // Emit mud tread tracks or ripped foundation scars when driving
        let speed = output.linear_velocity.length();
        vehicle.distance_since_last_tread_decal = sanitize_f32(
            vehicle.distance_since_last_tread_decal
                + (speed + output.angular_velocity.abs() * 18.0) * safe_dt,
            0.0,
        );
        if vehicle.distance_since_last_tread_decal >= 18.0 {
            vehicle.distance_since_last_tread_decal = 0.0;
            let pos_xy = pose.pos;
            let decal_kind = match *blade_state {
                BladeState::BladeUp => DecalKind::MudTreadTrack,
                BladeState::BladeDown => DecalKind::ConcreteFoundationScar,
            };
            decal_writer.send(DecalSpawnRequest {
                kind: decal_kind,
                world_pos: pos_xy,
                size: Vec2::new(16.0, 24.0),
                rotation_rad: vehicle.heading_radians,
            });
        }

        // Emit diesel exhaust smoke puffs from stack position when under load or throttle
        vehicle.exhaust_smoke_timer += safe_dt;
        let throttle_active =
            track_input.left_track.abs() > 0.1 || track_input.right_track.abs() > 0.1;
        if vehicle.exhaust_smoke_timer >= 0.22 && (throttle_active || stress.engine_heat_pct > 30.0)
        {
            vehicle.exhaust_smoke_timer = 0.0;
            let stack_world = heading_offset(pose.pos, pose.heading_rad, 9.0, -7.2);
            particle_writer.send(ParticleBurstEvent {
                kind: ParticleBurstKind::DieselExhaustSmoke,
                world_pos: stack_world,
                count: 2,
            });
        }
    }

    // Update 2.5D dozer assembly & blade visual when BladeState changes
    if blade_state.is_changed() {
        let (new_base, new_assembly) = build_dozer_iso_assembly(*blade_state);
        *base_box = new_base;
        *assembly = new_assembly;
    }

    if let Ok((mut b_tf, mut b_sprite)) = blade_visual_query.get_single_mut() {
        match *blade_state {
            BladeState::BladeUp => {
                b_tf.translation.x = 24.0;
                b_sprite.color = Color::NONE;
            }
            BladeState::BladeDown => {
                b_tf.translation.x = 28.5;
                b_sprite.color = Color::NONE;
            }
        }
    }

    if let Some(disp) = output.depenetration_displacement {
        let safe_disp = sanitize_vec2(disp, Vec2::ZERO);
        pose.pos = sanitize_vec2(pose.pos + safe_disp, pose.pos);
    }

    // 2. Write integrated Cartesian WorldPose into Avian Position/Rotation so collision solver
    // operates purely in Cartesian world space while Transform remains projected screen space.
    if let Some(pos_comp) = phys_pos.as_deref_mut() {
        pos_comp.0 = pose.pos;
    }
    if let Some(rot_comp) = phys_rot.as_deref_mut() {
        *rot_comp = Rotation::radians(pose.heading_rad);
    }
    lin_vel.0 = Vec2::ZERO;
    ang_vel.0 = 0.0;
    popup_pool.fallback_dozer_pos =
        world_to_iso_screen(pose.pos, pose.elev_z).extend(ISO_BIAS_DOZER);
}

/// Resets dozer gauges, blade state, and upgrade progression on `SiteRestartRequested`.
/// - Retrying a site (`preserve_layout == true`) restores `level_entry_upgrades`.
/// - Advancing to a new Campaign level (`preserve_layout == false` with changed `env_config.level`)
///   carries current installed upgrades forward into `level_entry_upgrades`.
/// - Starting a new game (`preserve_layout == false` on the same level) resets upgrades to `1.0x`.
pub fn reset_dozer_on_site_restart_system(
    mut restart_events: EventReader<SiteRestartRequested>,
    env_config: Res<EnvironmentConfig>,
    mut blade_state: ResMut<BladeState>,
    mut upgrades: ResMut<UpgradeMultipliers>,
    mut progression: ResMut<UpgradeProgression>,
    mut stress: ResMut<MechanicalStress>,
    mut track_input: ResMut<TrackInput>,
) {
    let mut triggered = false;
    let mut preserve_layout = false;
    for ev in restart_events.read() {
        triggered = true;
        preserve_layout = ev.preserve_layout;
    }
    if !triggered {
        return;
    }

    *blade_state = BladeState::BladeUp;
    if preserve_layout {
        progression.retry_level(&mut upgrades);
    } else if env_config.level != progression.active_level {
        progression.advance_level(&upgrades, env_config.level);
    } else {
        progression.reset_new_game(&mut upgrades, env_config.level);
    }
    stress.reset(env_config.mode == GameMode::Sandbox);
    *track_input = TrackInput::default();
}

/// Bevy plugin registering dozer physics, timed powered blade state, upgrade progression,
/// and mechanical stress systems.
pub struct PdDozerPlugin;

impl Plugin for PdDozerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BladeState>()
            .init_resource::<UpgradeMultipliers>()
            .init_resource::<UpgradeProgression>()
            .init_resource::<MechanicalStress>()
            .init_resource::<TrackInput>()
            .insert_resource(SyncConfig {
                position_to_transform: false,
                transform_to_position: false,
                transform_to_collider_scale: false,
            })
            .insert_resource(PrepareConfig {
                position_to_transform: false,
                transform_to_position: true,
            })
            .add_systems(
                Update,
                (
                    reset_dozer_on_site_restart_system,
                    ensure_player_dozer_spawned_system.run_if(in_state(AppState::Playing)),
                    read_dozer_keyboard_input_system.run_if(in_state(AppState::Playing)),
                    evaluate_upgrade_milestones_and_hotkeys_system,
                )
                    .chain()
                    .after(PdSystemSet::InputAndClock)
                    .before(PdSystemSet::WorldTilemapSync),
            )
            .add_systems(
                FixedUpdate,
                (
                    ensure_player_dozer_spawned_system,
                    fixed_dozer_kinematics_system,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing))
                    .in_set(PdSystemSet::DozerKinematics),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step_virtual_dozer(
        input: &mut TrackInput,
        upgrades: &UpgradeMultipliers,
        stress: &mut MechanicalStress,
        dt: f32,
    ) -> Result<KinematicOutput, PhysicsError> {
        let desired_blade = if input.blade_pressed {
            BladeState::BladeDown
        } else {
            BladeState::BladeUp
        };
        let out = compute_skid_steer_kinematics(input, desired_blade, upgrades, stress, dt)?;
        input.heading_radians += out.angular_velocity * dt.clamp(0.0001, 0.05);
        input.vx = out.linear_velocity.x;
        input.vy = out.linear_velocity.y;
        input.blade_down = out.blade_state == BladeState::BladeDown;
        input.push_timer_secs = out.push_timer_secs;
        input.push_cooldown_secs = out.push_cooldown_secs;
        input.blade_landed_this_tick = out.blade_landed_this_tick;
        input.pitch = out.pitch;
        input.pitch_vel = out.pitch_vel;
        input.roll = out.roll;
        input.roll_vel = out.roll_vel;
        input.odometer_dist = out.odometer_dist;
        Ok(out)
    }

    #[test]
    fn test_kinematic_extremes_and_watchdog() {
        let mut stress = MechanicalStress::default();
        let upgrades = UpgradeMultipliers {
            ar400_edge: f32::INFINITY,
            charge_air_kit: -99.0,
            aux_ram: f32::NAN,
        };
        let input = TrackInput {
            left_track: 1.0,
            right_track: 1.0,
            heading_radians: 0.0,
            external_load_factor: 2.0,
            penetration_depth: 0.8,
            contact_normal: Vec2::new(1.0, 0.0),
            ..Default::default()
        };

        // dt == 0.0 must return Err(PhysicsError::InvalidDeltaTime)
        assert_eq!(
            compute_skid_steer_kinematics(
                &input,
                BladeState::BladeDown,
                &upgrades,
                &mut stress,
                0.0
            ),
            Err(PhysicsError::InvalidDeltaTime(0.0))
        );

        // dt == 999.0 clamps to MAX_SAFE_DT (0.05) and produces finite velocities
        for tick in 1..=4 {
            let res = compute_skid_steer_kinematics(
                &input,
                BladeState::BladeDown,
                &upgrades,
                &mut stress,
                999.0,
            );
            assert!(res.is_ok());
            if let Ok(out) = res {
                assert!(out.linear_velocity.is_finite());
                assert!(out.angular_velocity.is_finite());
                assert!(out.pitch.is_finite());
                assert!(out.roll.is_finite());
                if tick == 4 {
                    // After >3 consecutive wedged ticks, watchdog triggers +0.5m depenetration
                    assert!(out.crush_intersecting_block);
                    assert_eq!(
                        out.depenetration_displacement,
                        Some(Vec2::new(DEPENETRATION_STEP_METERS, 0.0))
                    );
                }
            }
        }
    }

    #[test]
    fn test_sandbox_breakdown_immunity() {
        let mut stress = MechanicalStress {
            sandbox_mode: true,
            ..MechanicalStress::default()
        };
        let upgrades = UpgradeMultipliers::default();
        let input = TrackInput {
            left_track: 1.0,
            right_track: -1.0,
            heading_radians: 0.0,
            external_load_factor: 5.0,
            penetration_depth: 0.0,
            contact_normal: Vec2::Y,
            ..Default::default()
        };

        for _ in 0..500 {
            let res = compute_skid_steer_kinematics(
                &input,
                BladeState::BladeDown,
                &upgrades,
                &mut stress,
                0.05,
            );
            assert!(res.is_ok());
        }

        assert!(!stress.overheated);
        assert!(!stress.track_damaged);
        assert!(stress.engine_heat_pct <= 75.0);
        assert!(stress.track_stress_pct <= 75.0);
    }

    #[test]
    fn test_issue_11_timed_powered_blade_surge_and_5_blade_points() {
        let mut stress = MechanicalStress {
            sandbox_mode: true,
            ..MechanicalStress::default()
        };
        let upgrades = UpgradeMultipliers::from_levels(0, 0, 1); // Aux Ram +35% -> 1.35x
        let mut input = TrackInput::from_throttle_steer(1.0, 0.0, true);
        let dt = 1.0 / 60.0;

        // First tick with Space pressed triggers timed push (`push_t = 0.72 * 1.35 - dt`) and `blade_landed_this_tick`
        let res1 = step_virtual_dozer(&mut input, &upgrades, &mut stress, dt);
        assert!(res1.is_ok());
        if let Ok(out1) = res1 {
            assert_eq!(out1.blade_state, BladeState::BladeDown);
            assert!(out1.blade_landed_this_tick);
            assert!(out1.push_timer_secs > 0.90);
            assert!((out1.push_cooldown_secs - DOZER_PUSH_COOLDOWN).abs() < 1e-4);
            assert!(out1.pitch_vel > 0.0);
        }

        // Second tick: `blade_landed_this_tick` resets to false; release Space, blade stays down while `push_timer_secs > 0`
        input.blade_pressed = false;
        let res2 = step_virtual_dozer(&mut input, &upgrades, &mut stress, dt);
        assert!(res2.is_ok());
        if let Ok(out2) = res2 {
            assert!(!out2.blade_landed_this_tick);
            assert_eq!(out2.blade_state, BladeState::BladeDown);
        }

        // Drive forward while push is active: top speed can reach up to 1.12x BASE_TOP_SPEED
        input.blade_pressed = true;
        let mut peak_speed = 0.0_f32;
        for _ in 0..120 {
            if let Ok(out) = step_virtual_dozer(&mut input, &upgrades, &mut stress, dt) {
                peak_speed = peak_speed.max(out.linear_velocity.length());
            }
        }
        assert!(peak_speed > BASE_TOP_SPEED * 1.05);
        assert!(peak_speed <= BASE_TOP_SPEED * DOZER_PUSH_SPEED_CAP_MUL + 0.5);

        // Verify Aux Ram (+35%) produces strictly higher 1-tick powered-push surge acceleration than 1.0x baseline
        let base_up = UpgradeMultipliers::default();
        let mut base_in = TrackInput::from_throttle_steer(1.0, 0.0, true);
        let mut ram_in = TrackInput::from_throttle_steer(1.0, 0.0, true);
        let base_step1 = step_virtual_dozer(&mut base_in, &base_up, &mut stress, dt);
        let ram_step1 = step_virtual_dozer(&mut ram_in, &upgrades, &mut stress, dt);
        if let (Ok(b_out), Ok(r_out)) = (base_step1, ram_step1) {
            assert!(r_out.linear_velocity.x > b_out.linear_velocity.x + 1.5);
        }

        // Verify 5 blade contact points extend forward when blade_down is true and are always finite
        let pts_up = compute_blade_contact_points(Vec2::new(10.0, 20.0), 0.0, false);
        let pts_down = compute_blade_contact_points(Vec2::new(10.0, 20.0), 0.0, true);
        for (p_up, p_down) in pts_up.iter().zip(pts_down.iter()) {
            assert!(p_up.is_finite());
            assert!(p_down.is_finite());
            assert!((p_down.x - p_up.x - DOZER_BLADE_DOWN_EXTEND).abs() < 1e-3);
        }
        // Non-finite inputs to compute_blade_contact_points sanitize cleanly
        let pts_nan = compute_blade_contact_points(Vec2::splat(f32::NAN), f32::INFINITY, true);
        assert!(pts_nan.iter().all(|p| p.is_finite()));

        // Verify top grouser bars in build_dozer_iso_assembly sit at z0 = 9.4 (protruding above 9.5-unit track shell)
        let (_, asm) = build_dozer_iso_assembly(BladeState::BladeUp);
        let raised_grousers = asm
            .parts
            .iter()
            .filter(|b| (b.height - 0.9).abs() < 1e-3 && (b.z0 - 9.4).abs() < 1e-3)
            .count();
        assert_eq!(raised_grousers, 14);
    }

    #[test]
    fn test_issue_12_momentum_turn_scrub_and_chassis_spring_damper() {
        let mut stress = MechanicalStress {
            sandbox_mode: true,
            ..MechanicalStress::default()
        };
        let upgrades = UpgradeMultipliers::default();
        let dt = 1.0 / 60.0;

        // 1. Squat rear under forward throttle (`pitch < -0.01`), settle, then dip nose when braking (`pitch > 0.01`)
        let mut d = TrackInput::from_throttle_steer(1.0, 0.0, false);
        for _ in 0..6 {
            let _ = step_virtual_dozer(&mut d, &upgrades, &mut stress, dt);
        }
        assert!(d.pitch < -0.01);

        for _ in 0..120 {
            let _ = step_virtual_dozer(&mut d, &upgrades, &mut stress, dt);
        }
        assert!(d.pitch.abs() < 0.005);
        assert!((d.vx - BASE_TOP_SPEED).abs() < 0.5);

        // Brake / reverse: first braking tick preserves forward momentum (> 130.0) instead of clamping to BASE_MAX_REVERSE_SPEED (73.6)
        let rev_cmd = TrackInput::from_throttle_steer(-1.0, 0.0, false);
        d.left_track = rev_cmd.left_track;
        d.right_track = rev_cmd.right_track;
        let _ = step_virtual_dozer(&mut d, &upgrades, &mut stress, dt);
        assert!(d.vx > BASE_MAX_REVERSE_SPEED + 50.0);
        for _ in 0..5 {
            let _ = step_virtual_dozer(&mut d, &upgrades, &mut stress, dt);
        }
        assert!(d.pitch > 0.01);

        // Reverse acceleration from rest pitches nose down (`pitch > 0.008`), settles at steady
        // reverse speed, and braking from reverse pitches rear down / nose up (`pitch < -0.01`).
        let mut rev_d = TrackInput::from_throttle_steer(-1.0, 0.0, false);
        for _ in 0..6 {
            let _ = step_virtual_dozer(&mut rev_d, &upgrades, &mut stress, dt);
        }
        assert!(rev_d.pitch > 0.008);
        for _ in 0..120 {
            let _ = step_virtual_dozer(&mut rev_d, &upgrades, &mut stress, dt);
        }
        assert!(rev_d.pitch.abs() < 0.005);
        assert!((rev_d.vx + BASE_MAX_REVERSE_SPEED).abs() < 0.5);
        let fwd_cmd = TrackInput::from_throttle_steer(1.0, 0.0, false);
        rev_d.left_track = fwd_cmd.left_track;
        rev_d.right_track = fwd_cmd.right_track;
        for _ in 0..6 {
            let _ = step_virtual_dozer(&mut rev_d, &upgrades, &mut stress, dt);
        }
        assert!(rev_d.pitch < -0.01);

        // 2. Pivot turn scrubbing: turning while driving holds a lower speed than driving straight,
        // and coasting while turning sheds momentum faster than coasting straight.
        let mut run_speed = |throttle: f32, steer: f32, secs: f32, start_vx: f32| -> f32 {
            let mut st = TrackInput::from_throttle_steer(throttle, steer, false);
            st.vx = start_vx;
            let steps = (secs / dt).round() as usize;
            for _ in 0..steps {
                let _ = step_virtual_dozer(&mut st, &upgrades, &mut stress, dt);
            }
            Vec2::new(st.vx, st.vy).length()
        };

        let straight = run_speed(1.0, 0.0, 4.0, 0.0);
        let turning = run_speed(1.0, 1.0, 4.0, 0.0);
        assert!(turning < straight * 0.95);
        assert!(turning > straight * 0.75);

        let coast_straight = run_speed(0.0, 0.0, 0.5, 6.0 * WORLD_UNITS_PER_METER);
        let coast_turning = run_speed(0.0, 1.0, 0.5, 6.0 * WORLD_UNITS_PER_METER);
        assert!(coast_turning < coast_straight - 0.5 * WORLD_UNITS_PER_METER);

        // Steering-driven chassis roll leans into positive/negative turns and settles when straight
        let mut turn_left = TrackInput::from_throttle_steer(0.5, 1.0, false);
        let mut turn_right = TrackInput::from_throttle_steer(0.5, -1.0, false);
        for _ in 0..15 {
            let _ = step_virtual_dozer(&mut turn_left, &upgrades, &mut stress, dt);
            let _ = step_virtual_dozer(&mut turn_right, &upgrades, &mut stress, dt);
        }
        assert!(turn_left.roll > 0.008);
        assert!(turn_right.roll < -0.008);
        turn_left.left_track = 0.5;
        turn_left.right_track = 0.5;
        for _ in 0..90 {
            let _ = step_virtual_dozer(&mut turn_left, &upgrades, &mut stress, dt);
        }
        assert!(turn_left.roll.abs() < 0.002);

        // 3. Solid impact impulse pitches nose down on head-on hit and rolls hull on glancing hit,
        // while tangential motion (`vn == 0.0`) skips velocity loss and impact rocking.
        let mut tang_pos = Vec2::ZERO;
        let orig_tang_vel = Vec2::new(6.0 * WORLD_UNITS_PER_METER, 0.0);
        let mut tang_vel = orig_tang_vel;
        let mut tang_pitch = 0.0_f32;
        let mut tang_roll = 0.0_f32;
        let mut tang_last = 0.0_f32;
        let tang_impact = apply_dozer_impulse(
            &mut tang_pos,
            &mut tang_vel,
            0.0,
            &mut tang_pitch,
            &mut tang_roll,
            &mut tang_last,
            Vec2::new(0.0, 1.0),
            2.0,
            0.12,
        );
        assert!(tang_impact.abs() < 1e-6);
        assert_eq!(tang_vel, orig_tang_vel);
        assert!(tang_last.abs() < 1e-6);
        assert!(tang_pitch.abs() < 1e-6);
        assert!(tang_roll.abs() < 1e-6);

        let mut pos = Vec2::ZERO;
        let mut vel = Vec2::new(6.0 * WORLD_UNITS_PER_METER, 3.0 * WORLD_UNITS_PER_METER);
        let mut pitch = 0.0_f32;
        let mut roll = 0.0_f32;
        let mut last_impact = 0.0_f32;
        let impact_m = apply_dozer_impulse(
            &mut pos,
            &mut vel,
            0.0,
            &mut pitch,
            &mut roll,
            &mut last_impact,
            Vec2::new(-1.0, 0.0),
            2.0,
            0.12,
        );
        assert!(impact_m > 5.0);
        assert!(pitch > 0.0);
        assert!(pitch <= PITCH_MAX);
    }

    #[test]
    fn test_issue_13_milestone_upgrades_and_campaign_level_retry_vs_advance() {
        let mut upgrades = UpgradeMultipliers::default();
        let mut prog = UpgradeProgression::default();

        // Verify each AR400 Edge level (0 -> 1 -> 2 -> 3) and beyond 12 picks (15, 30, 60) scales node shear up to 50
        let mut veh0 = PlayerVehicle::default();
        let mut veh1 = PlayerVehicle::default();
        let mut veh2 = PlayerVehicle::default();
        let mut veh3 = PlayerVehicle::default();
        let mut veh15 = PlayerVehicle::default();
        let mut veh30 = PlayerVehicle::default();
        let mut veh60 = PlayerVehicle::default();
        let up0 = UpgradeMultipliers::from_levels(0, 0, 0);
        let up1 = UpgradeMultipliers::from_levels(1, 0, 0);
        let up2 = UpgradeMultipliers::from_levels(2, 0, 0);
        let up3 = UpgradeMultipliers::from_levels(3, 0, 0);
        let up15 = UpgradeMultipliers::from_levels(15, 0, 0);
        let up30 = UpgradeMultipliers::from_levels(30, 0, 0);
        let up60 = UpgradeMultipliers::from_levels(60, 0, 0);
        assert_eq!(veh0.consume_blade_shear_nodes(BladeState::BladeDown, &up0), 2);
        assert_eq!(veh1.consume_blade_shear_nodes(BladeState::BladeDown, &up1), 3);
        assert_eq!(veh2.consume_blade_shear_nodes(BladeState::BladeDown, &up2), 4);
        assert_eq!(veh3.consume_blade_shear_nodes(BladeState::BladeDown, &up3), 5);
        assert_eq!(veh15.consume_blade_shear_nodes(BladeState::BladeDown, &up15), 15);
        assert_eq!(veh30.consume_blade_shear_nodes(BladeState::BladeDown, &up30), 27);
        assert_eq!(veh60.consume_blade_shear_nodes(BladeState::BladeDown, &up60), 50);

        // Verify Charge-Air Kit (+28% per level) scales both reverse acceleration and reverse top speed
        let mut stress = MechanicalStress {
            sandbox_mode: true,
            ..MechanicalStress::default()
        };
        let dt = 1.0 / 60.0;
        let base_engine = UpgradeMultipliers::from_levels(0, 0, 0);
        let upgraded_engine = UpgradeMultipliers::from_levels(0, 2, 0); // 1.56x engine multiplier
        let mut rev_base = TrackInput::from_throttle_steer(-1.0, 0.0, false);
        let mut rev_upgraded = TrackInput::from_throttle_steer(-1.0, 0.0, false);
        let _ = step_virtual_dozer(&mut rev_base, &base_engine, &mut stress, dt);
        let _ = step_virtual_dozer(&mut rev_upgraded, &upgraded_engine, &mut stress, dt);
        assert!(rev_upgraded.vx < rev_base.vx - 0.5);
        for _ in 0..180 {
            let _ = step_virtual_dozer(&mut rev_base, &base_engine, &mut stress, dt);
            let _ = step_virtual_dozer(&mut rev_upgraded, &upgraded_engine, &mut stress, dt);
        }
        assert!((rev_base.vx.abs() - BASE_MAX_REVERSE_SPEED).abs() < 0.5);
        assert!(
            (rev_upgraded.vx.abs() - BASE_MAX_REVERSE_SPEED * upgraded_engine.charge_air_kit).abs()
                < 0.5
        );

        // Cross first milestone ($350) in Campaign mode
        assert_eq!(prog.evaluate_cash_milestones(350), 1);
        assert_eq!(prog.pending_upgrade_choices, 1);
        assert_eq!(prog.next_milestone_cash, 900);

        // Pick 1 (AR400 Edge +42%)
        assert!(prog.try_apply_choice(&mut upgrades, GameMode::Campaign, UpgradeChoice::Ar400Edge));
        assert_eq!(prog.pending_upgrade_choices, 0);
        assert!((upgrades.ar400_edge - 1.42).abs() < 1e-4);

        // Cannot pick again in Campaign mode without another pending choice
        assert!(!prog.try_apply_choice(
            &mut upgrades,
            GameMode::Campaign,
            UpgradeChoice::ChargeAirKit
        ));

        // Clearing Level 1 -> advancing to Level 2 preserves 1.42x into level_entry_upgrades
        prog.advance_level(&upgrades, CampaignLevel::Village2);
        assert!((prog.level_entry_upgrades.ar400_edge - 1.42).abs() < 1e-4);

        // Earn another milestone on Level 2 and pick Charge-Air Kit (+28%)
        assert_eq!(prog.evaluate_cash_milestones(400), 1);
        assert!(prog.try_apply_choice(
            &mut upgrades,
            GameMode::Campaign,
            UpgradeChoice::ChargeAirKit
        ));
        assert!((upgrades.charge_air_kit - 1.28).abs() < 1e-4);

        // Retrying Level 2 restores Level 2's entry upgrades (ar400_edge = 1.42, charge_air_kit = 1.0)
        prog.retry_level(&mut upgrades);
        assert!((upgrades.ar400_edge - 1.42).abs() < 1e-4);
        assert!((upgrades.charge_air_kit - 1.0).abs() < 1e-4);

        // In Sandbox mode, upgrades can be applied immediately without pending milestones
        assert!(prog.try_apply_choice(&mut upgrades, GameMode::Sandbox, UpgradeChoice::AuxRam));
        assert!((upgrades.aux_ram - 1.35).abs() < 1e-4);
    }
}
