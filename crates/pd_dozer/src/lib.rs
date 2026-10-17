//! `pd_dozer`: Armored bulldozer skid-steer physics, `BLADE UP` / `BLADE DOWN` hydraulic state
//! machine, `ENGINE HEAT` / `TRACK STRESS` gauges, upgrade multipliers (`AR400 Edge`,
//! `Charge-Air Kit`, `Aux Ram`), multi-part 2.5D dozer sprite hierarchy, tread decals,
//! and anti-wedging depenetration watchdog.
//!
//! Depends only on `pd_core`.

use avian2d::{ prelude::*, prepare::PrepareConfig, sync::SyncConfig };
use bevy::prelude::*;
use pd_core::{
    AppState, AudioSfxKind, AudioTriggerEvent, CashLedger, CashPopupPool, DecalKind,
    DecalSpawnRequest, EnvironmentConfig, GameMode, ISO_BIAS_DOZER, IsoBoxAssembly,
    IsoExtrudedBox, IsoLayerBias, ParticleBurstEvent, ParticleBurstKind, PdSystemSet,
    PhysicsError, SessionEpoch, SiteRestartRequested, SiteScoped, WorldPose, heading_offset,
    iso_depth_key, sanitize_dt, sanitize_f32, sanitize_vec2, world_to_iso_screen,
};
use serde::{Deserialize, Serialize};

/// Maximum allowed upgrade multiplier to prevent infinite velocity/impulse explosions.
pub const MAX_SAFE_MULTIPLIER: f32 = 10.0;

/// Maximum safe impulse applied to the dozer per fixed timestep.
pub const MAX_DOZER_IMPULSE_PER_TICK: f32 = 450_000.0;

/// Penetration threshold (meters) triggering the anti-wedging depenetration watchdog.
pub const MAX_SAFE_PENETRATION_DEPTH: f32 = 0.35;

/// Number of consecutive wedged fixed ticks before auto-crushing/depenetrating (+0.5m).
pub const WEDGED_TICK_THRESHOLD: u32 = 3;

/// Depenetration recovery step (meters) along contact normal.
pub const DEPENETRATION_STEP_METERS: f32 = 0.5;

/// Baseline forward speed (pixels/meters per second in 2.5D space) with Blade Up.
pub const BASE_TOP_SPEED: f32 = 140.0;

/// Baseline zero-turn yaw rate (radians per second).
pub const BASE_YAW_RATE: f32 = 2.6;

/// Marker component for the player's armored bulldozer entity.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct PlayerVehicle {
    pub heading_radians: f32,
    pub distance_since_last_tread_decal: f32,
    pub exhaust_smoke_timer: f32,
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

/// Live upgrade multipliers tracked on the cab dashboard HUD and scaled via cash milestones.
#[derive(Component, Resource, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct UpgradeMultipliers {
    /// Multiplies structural shear damage on blade contact (`1.00x` to `4.36x+`).
    pub ar400_edge: f32,
    /// Increases horsepower, top speed, and heat dissipation (`1.00x` to `3.24x+`).
    pub charge_air_kit: f32,
    /// Increases hydraulic pushing force and knockback mass (`1.00x` to `3.80x+`).
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

    /// Updates live upgrade multipliers deterministically from current demolition cash.
    pub fn sync_from_cash(&mut self, cash: u32) {
        let ratio = (cash as f32 / 150_000.0).clamp(0.0, 3.0);
        self.ar400_edge = (1.0 + ratio * 1.12).clamp(1.0, MAX_SAFE_MULTIPLIER);
        self.charge_air_kit = (1.0 + ratio * 0.75).clamp(1.0, MAX_SAFE_MULTIPLIER);
        self.aux_ram = (1.0 + ratio * 0.93).clamp(1.0, MAX_SAFE_MULTIPLIER);
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

/// Differential track input and contact telemetry fed into `compute_skid_steer_kinematics`.
#[derive(Component, Resource, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackInput {
    pub left_track: f32,
    pub right_track: f32,
    pub heading_radians: f32,
    pub external_load_factor: f32,
    pub penetration_depth: f32,
    pub contact_normal: Vec2,
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
        }
    }
}

/// Validated kinematic and mechanical output produced each fixed timestep.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KinematicOutput {
    pub linear_velocity: Vec2,
    pub angular_velocity: f32,
    pub blade_shear_multiplier: f32,
    pub pushing_force_multiplier: f32,
    pub depenetration_displacement: Option<Vec2>,
    pub crush_intersecting_block: bool,
}

/// Contract Function (Module B): Calculates differential left/right track forces, applies drag
/// penalties when the blade is lowered, scales output by `Charge-Air Kit` and `Aux Ram`
/// multipliers, updates `ENGINE HEAT` and `TRACK STRESS` percentages, and enforces the
/// anti-wedging depenetration watchdog.
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

    let left = input.left_track.clamp(-1.0, 1.0);
    let right = input.right_track.clamp(-1.0, 1.0);
    let load = sanitize_f32(input.external_load_factor, 0.0).clamp(0.0, 5.0);

    // 2. Sanitize upgrades multipliers: clamp each multiplier to [1.0, MAX_SAFE_MULTIPLIER].
    let safe_upgrades = upgrades.sanitized();

    // 3. Blade state drag and shear scaling.
    let (blade_speed_drag, blade_shear_multiplier, blade_stress_factor) = match blade_state {
        BladeState::BladeUp => (1.0_f32, 1.0_f32, 0.6_f32),
        BladeState::BladeDown => (0.74_f32, safe_upgrades.ar400_edge, 1.45_f32),
    };

    // 4. Update Engine Heat & Track Stress percentages.
    let throttle_effort = (left.abs() + right.abs()) * 0.5;
    let turn_shear = (right - left).abs() * 0.5;

    let heat_gain = (throttle_effort * 6.5 + load * 14.0 * blade_stress_factor) * safe_dt;
    let heat_cool = (4.5 * safe_upgrades.charge_air_kit) * (1.15 - throttle_effort * 0.6) * safe_dt;
    let raw_heat = sanitize_f32(stress.engine_heat_pct, 0.0) + heat_gain - heat_cool;

    let stress_gain =
        (turn_shear * 11.0 * blade_stress_factor + load * 16.0 * blade_stress_factor) * safe_dt;
    let stress_cool = (6.0 * safe_upgrades.aux_ram.sqrt()) * (1.0 - turn_shear * 0.5) * safe_dt;
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

    // Apply breakdown speed penalty in Campaign mode if overheated or track-stressed.
    let breakdown_factor = if !stress.sandbox_mode && (stress.overheated || stress.track_damaged) {
        0.25_f32
    } else {
        1.0_f32
    };

    // Differential skid-steer kinematics.
    let forward_drive = (left + right) * 0.5;
    let turn_drive = (right - left) * 0.5;

    let effective_speed = BASE_TOP_SPEED
        * safe_upgrades.charge_air_kit.sqrt()
        * blade_speed_drag
        * breakdown_factor;
    let forward_dir = Vec2::new(input.heading_radians.cos(), input.heading_radians.sin());
    let linear_velocity = forward_dir * (forward_drive * effective_speed);
    let angular_velocity =
        turn_drive * BASE_YAW_RATE * blade_speed_drag * breakdown_factor;

    // Anti-wedging & depenetration watchdog.
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

    // 5. Verify output linear and angular velocity vectors are finite before returning.
    if !linear_velocity.is_finite() || !angular_velocity.is_finite() {
        return Err(PhysicsError::NonFiniteOutput);
    }

    Ok(KinematicOutput {
        linear_velocity,
        angular_velocity,
        blade_shear_multiplier,
        pushing_force_multiplier: safe_upgrades.aux_ram,
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
            27.5_f32,
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

/// Reads keyboard input to update `TrackInput` and toggle `BladeState`.
pub fn read_dozer_keyboard_input_system(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut track_input: ResMut<TrackInput>,
    mut blade_state: ResMut<BladeState>,
    mut audio_events: EventWriter<AudioTriggerEvent>,
) {
    let Some(keys) = keys else {
        return;
    };

    if keys.just_pressed(KeyCode::Space)
        || keys.just_pressed(KeyCode::KeyR)
        || keys.just_pressed(KeyCode::KeyF)
        || keys.just_pressed(KeyCode::ShiftLeft)
    {
        *blade_state = blade_state.toggle();
        audio_events.send(AudioTriggerEvent {
            kind: AudioSfxKind::BladeHydraulicHiss,
        });
    }

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

    // Direct independent track overrides (Q/Z left track, E/C right track)
    let mut left = (forward - turn).clamp(-1.0, 1.0);
    let mut right = (forward + turn).clamp(-1.0, 1.0);

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

/// Executes fixed-timestep skid-steer kinematics in Cartesian `WorldPose` coordinates,
/// synchronizes solver corrections with Cartesian `Position`/`Rotation`, updates dozer velocity &
/// HUD telemetry, animates the hydraulic blade visual, and emits persistent tread track decals +
/// diesel smoke.
pub fn fixed_dozer_kinematics_system(
    time: Res<Time>,
    env_config: Res<EnvironmentConfig>,
    ledger: Res<CashLedger>,
    mut popup_pool: ResMut<CashPopupPool>,
    mut track_input: ResMut<TrackInput>,
    blade_state: Res<BladeState>,
    mut upgrades: ResMut<UpgradeMultipliers>,
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
    mut blade_visual_query: Query<(&mut Transform, &mut Sprite), (With<DozerBladeVisual>, Without<PlayerVehicle>)>,
) {
    stress.sandbox_mode = env_config.mode == GameMode::Sandbox;
    upgrades.sync_from_cash(ledger.current_cash);

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

    track_input.heading_radians = vehicle.heading_radians;

    let Ok(output) = compute_skid_steer_kinematics(
        &track_input,
        *blade_state,
        &upgrades,
        &mut stress,
        time.delta_secs(),
    ) else {
        lin_vel.0 = Vec2::ZERO;
        ang_vel.0 = 0.0;
        return;
    };

    if let Ok(safe_dt) = sanitize_dt(time.delta_secs()) {
        vehicle.heading_radians = sanitize_f32(
            vehicle.heading_radians + output.angular_velocity * safe_dt,
            vehicle.heading_radians,
        );
        pose.heading_rad = vehicle.heading_radians;
        pose.pos = sanitize_vec2(pose.pos + output.linear_velocity * safe_dt, pose.pos);

        // Subtle chassis pitch & roll from throttle and turning
        let forward_drive = (track_input.left_track + track_input.right_track) * 0.5;
        let turn_drive = (track_input.right_track - track_input.left_track) * 0.5;
        let target_pitch = (-forward_drive * 0.035).clamp(-0.08, 0.08);
        let target_roll = (turn_drive * 0.04).clamp(-0.08, 0.08);
        pose.pitch_rad = sanitize_f32(
            pose.pitch_rad + (target_pitch - pose.pitch_rad) * (safe_dt * 10.0).min(1.0),
            0.0,
        );
        pose.roll_rad = sanitize_f32(
            pose.roll_rad + (target_roll - pose.roll_rad) * (safe_dt * 10.0).min(1.0),
            0.0,
        );

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

/// Resets dozer gauges, upgrades, and blade state on `SiteRestartRequested`.
pub fn reset_dozer_on_site_restart_system(
    mut restart_events: EventReader<SiteRestartRequested>,
    env_config: Res<EnvironmentConfig>,
    mut blade_state: ResMut<BladeState>,
    mut upgrades: ResMut<UpgradeMultipliers>,
    mut stress: ResMut<MechanicalStress>,
    mut track_input: ResMut<TrackInput>,
) {
    let mut triggered = false;
    for _ in restart_events.read() {
        triggered = true;
    }
    if !triggered {
        return;
    }

    *blade_state = BladeState::BladeUp;
    upgrades.reset();
    stress.reset(env_config.mode == GameMode::Sandbox);
    *track_input = TrackInput::default();
}

/// Bevy plugin registering dozer physics, blade state, and mechanical stress systems.
pub struct PdDozerPlugin;

impl Plugin for PdDozerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BladeState>()
            .init_resource::<UpgradeMultipliers>()
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
}
