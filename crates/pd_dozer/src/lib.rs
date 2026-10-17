//! `pd_dozer`: Armored bulldozer skid-steer physics, `BLADE UP` / `BLADE DOWN` hydraulic state
//! machine, `ENGINE HEAT` / `TRACK STRESS` gauges, upgrade multipliers (`AR400 Edge`,
//! `Charge-Air Kit`, `Aux Ram`), multi-part 2.5D dozer sprite hierarchy, tread decals,
//! and anti-wedging depenetration watchdog.
//!
//! Depends only on `pd_core`.

use avian2d::prelude::*;
use bevy::prelude::*;
use pd_core::{
    AppState, AudioSfxKind, AudioTriggerEvent, CashLedger, CashPopupPool, DecalKind,
    DecalSpawnRequest, EnvironmentConfig, GameMode, ParticleBurstEvent, ParticleBurstKind,
    PdSystemSet, PhysicsError, SessionEpoch, SiteRestartRequested, SiteScoped, sanitize_dt,
    sanitize_f32, sanitize_vec2,
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

/// Spawns the player's armored bulldozer with a detailed multi-part 2.5D sprite hierarchy
/// (left/right armored tracks, industrial gold chassis, armored cab, and hydraulic AR400 blade).
pub fn ensure_player_dozer_spawned_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    existing_dozer: Query<Entity, With<PlayerVehicle>>,
) {
    if existing_dozer.iter().next().is_some() {
        return;
    }

    commands
        .spawn((
            Name::new("Armored Dozer (PlayerVehicle)"),
            PlayerVehicle::default(),
            SiteScoped { epoch: *epoch },
            Sprite {
                color: Color::srgb(0.88, 0.66, 0.14),
                custom_size: Some(Vec2::new(42.0, 26.0)),
                ..Default::default()
            },
            Transform::from_xyz(0.0, -60.0, 25.0),
            GlobalTransform::default(),
            RigidBody::Dynamic,
            Collider::rectangle(46.0, 30.0),
            LinearVelocity::default(),
            AngularVelocity::default(),
        ))
        .with_children(|parent| {
            // Left Armored Track
            parent.spawn((
                Name::new("Left Track Assembly"),
                Sprite {
                    color: Color::srgb(0.18, 0.19, 0.21),
                    custom_size: Some(Vec2::new(44.0, 7.0)),
                    ..Default::default()
                },
                Transform::from_xyz(0.0, 14.0, -0.5),
                GlobalTransform::default(),
            ));
            // Right Armored Track
            parent.spawn((
                Name::new("Right Track Assembly"),
                Sprite {
                    color: Color::srgb(0.18, 0.19, 0.21),
                    custom_size: Some(Vec2::new(44.0, 7.0)),
                    ..Default::default()
                },
                Transform::from_xyz(0.0, -14.0, -0.5),
                GlobalTransform::default(),
            ));
            // Reinforced Steel Cab Roof & Slit Visor
            parent.spawn((
                Name::new("Armored Operator Cab"),
                Sprite {
                    color: Color::srgb(0.26, 0.28, 0.30),
                    custom_size: Some(Vec2::new(18.0, 18.0)),
                    ..Default::default()
                },
                Transform::from_xyz(-6.0, 0.0, 1.0),
                GlobalTransform::default(),
            ));
            // Articulating Hydraulic AR400 Edge Blade
            parent.spawn((
                Name::new("AR400 Hydraulic Dozer Blade"),
                DozerBladeVisual,
                Sprite {
                    color: Color::srgb(0.76, 0.78, 0.82),
                    custom_size: Some(Vec2::new(10.0, 38.0)),
                    ..Default::default()
                },
                Transform::from_xyz(25.0, 0.0, 0.8),
                GlobalTransform::default(),
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

/// Executes fixed-timestep skid-steer kinematics, updates dozer velocity & HUD telemetry,
/// animates the hydraulic blade visual, and emits persistent tread track decals + diesel smoke.
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
        &mut Transform,
        &mut LinearVelocity,
        &mut AngularVelocity,
    )>,
    mut blade_visual_query: Query<(&mut Transform, &mut Sprite), (With<DozerBladeVisual>, Without<PlayerVehicle>)>,
) {
    stress.sandbox_mode = env_config.mode == GameMode::Sandbox;
    upgrades.sync_from_cash(ledger.current_cash);

    let Ok((mut vehicle, mut transform, mut lin_vel, mut ang_vel)) =
        dozer_query.get_single_mut()
    else {
        return;
    };

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
        transform.rotation = Quat::from_rotation_z(vehicle.heading_radians);

        // Emit mud tread tracks or ripped foundation scars when driving
        let speed = output.linear_velocity.length();
        vehicle.distance_since_last_tread_decal = sanitize_f32(
            vehicle.distance_since_last_tread_decal
                + (speed + output.angular_velocity.abs() * 18.0) * safe_dt,
            0.0,
        );
        if vehicle.distance_since_last_tread_decal >= 18.0 {
            vehicle.distance_since_last_tread_decal = 0.0;
            let pos_xy = transform.translation.truncate();
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

        // Emit diesel exhaust smoke puffs when under load or throttle
        vehicle.exhaust_smoke_timer += safe_dt;
        let throttle_active =
            track_input.left_track.abs() > 0.1 || track_input.right_track.abs() > 0.1;
        if vehicle.exhaust_smoke_timer >= 0.22 && (throttle_active || stress.engine_heat_pct > 30.0)
        {
            vehicle.exhaust_smoke_timer = 0.0;
            particle_writer.send(ParticleBurstEvent {
                kind: ParticleBurstKind::DieselExhaustSmoke,
                world_pos: transform.translation.truncate(),
                count: 2,
            });
        }
    }

    // Update hydraulic blade visual position & color based on BladeState
    if let Ok((mut b_tf, mut b_sprite)) = blade_visual_query.get_single_mut() {
        match *blade_state {
            BladeState::BladeUp => {
                b_tf.translation.x = 24.0;
                b_sprite.color = Color::srgb(0.76, 0.78, 0.82);
            }
            BladeState::BladeDown => {
                b_tf.translation.x = 28.5;
                b_sprite.color = Color::srgb(0.96, 0.74, 0.22);
            }
        }
    }

    if let Some(disp) = output.depenetration_displacement {
        let safe_disp = sanitize_vec2(disp, Vec2::ZERO);
        transform.translation.x = sanitize_f32(
            transform.translation.x + safe_disp.x,
            transform.translation.x,
        );
        transform.translation.y = sanitize_f32(
            transform.translation.y + safe_disp.y,
            transform.translation.y,
        );
    }

    lin_vel.0 = sanitize_vec2(output.linear_velocity, Vec2::ZERO);
    ang_vel.0 = sanitize_f32(output.angular_velocity, 0.0);
    popup_pool.fallback_dozer_pos = transform.translation;
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
