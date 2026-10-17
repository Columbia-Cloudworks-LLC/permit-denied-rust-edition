//! `pd_core`: Foundational state machine, `SessionEpoch`, economy ledger, floating cash popup
//! pool, environment configurations, and defensive math/float guards.
//!
//! Depends on NO internal workspace crates (Bottom layer of the DAG).

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Maximum 6-digit mechanical odometer ceiling (`$999,999`).
pub const MAX_ODOMETER_CASH: u32 = 999_999;

/// Maximum simultaneous floating world-space cash popups (`+$X`).
pub const MAX_CASH_POPUPS: usize = 50;

/// Spatial coalescing radius (meters) for simultaneous demolition cash events.
pub const COALESCE_RADIUS_METERS: f32 = 3.0;

/// Spatial coalescing radius (meters) when the popup pool is at full capacity.
pub const FULL_POOL_COALESCE_RADIUS_METERS: f32 = 2.0;

/// Temporal coalescing window (seconds) for simultaneous demolition cash events.
pub const COALESCE_WINDOW_SECS: f32 = 0.10;

/// Default lifetime (seconds) for a floating cash popup.
pub const DEFAULT_POPUP_LIFETIME_SECS: f32 = 1.6;

/// Minimum allowed fixed delta-time (seconds).
pub const MIN_SAFE_DT: f32 = 0.0001;

/// Maximum allowed fixed delta-time (seconds) to prevent lag-spike physics explosions.
pub const MAX_SAFE_DT: f32 = 0.05;

/// Top-level application state machine.
#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum AppState {
    #[default]
    Title,
    NewGameModal,
    Playing,
    Paused,
}

/// System sets for deterministic ordering across schedules.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PdSystemSet {
    InputAndClock,
    DozerKinematics,
    DestructionGraph,
    DebrisLifecycle,
    CrowdAndTrafficAi,
    WorldTilemapSync,
    HudInterpolation,
}

/// Game mode selection per Section 5 of the PRD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum GameMode {
    #[default]
    Campaign,
    Sandbox,
}

impl GameMode {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Campaign => "Campaign",
            Self::Sandbox => "Sandbox",
        }
    }
}

/// Campaign levels (`1/7 County` through `7/7 Governor's Mansion`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CampaignLevel {
    #[default]
    County1,
    Village2,
    Township3,
    Suburb4,
    CityBorough5,
    CityDowntown6,
    GovernorsMansion7,
}

impl CampaignLevel {
    pub const ALL: [Self; 7] = [
        Self::County1,
        Self::Village2,
        Self::Township3,
        Self::Suburb4,
        Self::CityBorough5,
        Self::CityDowntown6,
        Self::GovernorsMansion7,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::County1 => "1/7 County",
            Self::Village2 => "2/7 Village",
            Self::Township3 => "3/7 Township",
            Self::Suburb4 => "4/7 Suburb",
            Self::CityBorough5 => "5/7 City Borough",
            Self::CityDowntown6 => "6/7 City Downtown",
            Self::GovernorsMansion7 => "7/7 Governor's Mansion",
        }
    }

    #[must_use]
    pub const fn index_one_based(self) -> u8 {
        match self {
            Self::County1 => 1,
            Self::Village2 => 2,
            Self::Township3 => 3,
            Self::Suburb4 => 4,
            Self::CityBorough5 => 5,
            Self::CityDowntown6 => 6,
            Self::GovernorsMansion7 => 7,
        }
    }

    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::County1 => Self::Village2,
            Self::Village2 => Self::Township3,
            Self::Township3 => Self::Suburb4,
            Self::Suburb4 => Self::CityBorough5,
            Self::CityBorough5 => Self::CityDowntown6,
            Self::CityDowntown6 => Self::GovernorsMansion7,
            Self::GovernorsMansion7 => Self::County1,
        }
    }
}

/// Seasonal setting (`Random`, `Spring`, `Summer`, `Autumn`, `Winter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Season {
    #[default]
    Random,
    Spring,
    Summer,
    Autumn,
    Winter,
}

impl Season {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Random => "Random",
            Self::Spring => "Spring",
            Self::Summer => "Summer",
            Self::Autumn => "Autumn",
            Self::Winter => "Winter",
        }
    }

    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Random => Self::Spring,
            Self::Spring => Self::Summer,
            Self::Summer => Self::Autumn,
            Self::Autumn => Self::Winter,
            Self::Winter => Self::Random,
        }
    }

    /// Resolves `Season::Random` deterministically from a `SessionEpoch`.
    #[must_use]
    pub const fn resolve(self, epoch: u64) -> Self {
        match self {
            Self::Random => match epoch % 4 {
                0 => Self::Spring,
                1 => Self::Summer,
                2 => Self::Autumn,
                _ => Self::Winter,
            },
            concrete => concrete,
        }
    }
}

/// Weather toggle (`On` / `Off`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum WeatherSetting {
    #[default]
    On,
    Off,
}

impl WeatherSetting {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::On => "On",
            Self::Off => "Off",
        }
    }

    #[must_use]
    pub const fn toggle(self) -> Self {
        match self {
            Self::On => Self::Off,
            Self::Off => Self::On,
        }
    }
}

/// Active environment and campaign configuration resource.
#[derive(Resource, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentConfig {
    pub mode: GameMode,
    pub level: CampaignLevel,
    pub season: Season,
    pub resolved_season: Season,
    pub weather: WeatherSetting,
}

impl Default for EnvironmentConfig {
    fn default() -> Self {
        Self {
            mode: GameMode::Campaign,
            level: CampaignLevel::County1,
            season: Season::Random,
            resolved_season: Season::Spring,
            weather: WeatherSetting::On,
        }
    }
}

/// Monotonically increasing session generation counter that guards against stale async tasks
/// or deferred commands after `Restart Site` or `New Game`.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
pub struct SessionEpoch(pub u64);

impl SessionEpoch {
    /// Increments the epoch using saturating addition.
    pub fn advance(&mut self) -> Self {
        self.0 = self.0.saturating_add(1);
        *self
    }
}

/// Marker component placed on every entity spawned for a demolition site (buildings, chunks,
/// furniture, vehicles, NPCs, decals) so `Restart Site` deterministically cleans up the world.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SiteScoped {
    pub epoch: SessionEpoch,
}

/// Event requesting an idempotent `Restart Site` or `New Game` reset.
#[derive(Event, Debug, Clone, Copy)]
pub struct SiteRestartRequested {
    pub preserve_layout: bool,
}

/// Event fired when a structural node, prop, or vehicle is destroyed for cash.
#[derive(Event, Debug, Clone, Copy)]
pub struct DemolitionCashEvent {
    pub amount: u32,
    pub world_pos: Vec3,
}

/// Persistent ground decal types rendered above the base isometric tilemap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecalKind {
    MudTreadTrack,
    OilSlick,
    ScorchMark,
    ConcreteFoundationScar,
    CrushedRubbleScar,
}

/// Event requesting a persistent ground decal on the dynamic decal layer.
#[derive(Event, Debug, Clone, Copy)]
pub struct DecalSpawnRequest {
    pub kind: DecalKind,
    pub world_pos: Vec2,
    pub size: Vec2,
    pub rotation_rad: f32,
}

/// Particle burst effect categories (water burst on toilet/hydrant, propane explosion, exhaust smoke).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ParticleBurstKind {
    WaterSpray,
    PropaneExplosion,
    DieselExhaustSmoke,
    MasonryDust,
}

/// Event requesting a pooled world-space particle burst.
#[derive(Event, Debug, Clone, Copy)]
pub struct ParticleBurstEvent {
    pub kind: ParticleBurstKind,
    pub world_pos: Vec2,
    pub count: u8,
}

/// Synthesized sound effect categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioSfxKind {
    DieselRev,
    BladeHydraulicHiss,
    StructuralCrunch,
    CollapseRumble,
    PropaneBlast,
    CashDing,
}

/// Event triggering synthesized sound effects.
#[derive(Event, Debug, Clone, Copy)]
pub struct AudioTriggerEvent {
    pub kind: AudioSfxKind,
}

/// Cross-crate telemetry resource updated by `pd_destruction` / `pd_ai` and read by `pd_ui`.
#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize)]
pub struct DebugTelemetry {
    pub debug_overlay_visible: bool,
    pub sound_enabled: bool,
    pub active_physics_debris: usize,
    pub sleeping_rubble_count: usize,
    pub crushed_decal_count: usize,
    pub active_bfs_tasks: usize,
    pub intact_structural_nodes: usize,
    pub panicked_pedestrians: usize,
    pub info_banner: Option<String>,
}

/// Active building collapse zone broadcast to crowd and traffic AI systems.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CollapseZone {
    pub center: Vec2,
    pub radius: f32,
    pub epoch: SessionEpoch,
}

/// Resource tracking active collapse zones in the current frame.
#[derive(Resource, Debug, Clone, Default)]
pub struct ActiveCollapseZones {
    pub zones: Vec<CollapseZone>,
}

/// Error types for `CashLedger` and `CashPopupPool` operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerError {
    PoolCorrupted,
}

/// Error types for physics & kinematic validation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PhysicsError {
    InvalidDeltaTime(f32),
    NonFiniteInput,
    NonFiniteOutput,
}

/// Clamps and validates a frame/fixed delta-time to `[MIN_SAFE_DT, MAX_SAFE_DT]`.
pub fn sanitize_dt(dt: f32) -> Result<f32, PhysicsError> {
    if !dt.is_finite() || dt <= 0.0 {
        return Err(PhysicsError::InvalidDeltaTime(dt));
    }
    Ok(dt.clamp(MIN_SAFE_DT, MAX_SAFE_DT))
}

/// Returns `value` if finite, otherwise logs a warning and returns `fallback`.
#[must_use]
pub fn sanitize_f32(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else if fallback.is_finite() {
        warn!("Detected non-finite f32; falling back to safe scalar.");
        fallback
    } else {
        0.0
    }
}

/// Returns `vec` if finite, otherwise logs a warning and returns `fallback`.
#[must_use]
pub fn sanitize_vec2(vec: Vec2, fallback: Vec2) -> Vec2 {
    if vec.is_finite() {
        vec
    } else if fallback.is_finite() {
        warn!("Detected non-finite Vec2; falling back to safe vector.");
        fallback
    } else {
        Vec2::ZERO
    }
}

/// Returns `vec` if finite, otherwise logs a warning and returns `fallback`.
#[must_use]
pub fn sanitize_vec3(vec: Vec3, fallback: Vec3) -> Vec3 {
    if vec.is_finite() {
        vec
    } else if fallback.is_finite() {
        warn!("Detected non-finite Vec3; falling back to safe vector.");
        fallback
    } else {
        Vec3::ZERO
    }
}

/// 2.5D Cartesian world-space simulation pose decoupled from screen-space isometric `Transform`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct WorldPose {
    pub pos: Vec2,
    pub elev_z: f32,
    pub heading_rad: f32,
    pub pitch_rad: f32,
    pub roll_rad: f32,
}

impl WorldPose {
    #[must_use]
    pub fn new(pos: Vec2, elev_z: f32, heading_rad: f32) -> Self {
        Self {
            pos: sanitize_vec2(pos, Vec2::ZERO),
            elev_z: sanitize_f32(elev_z, 0.0),
            heading_rad: sanitize_f32(heading_rad, 0.0),
            pitch_rad: 0.0,
            roll_rad: 0.0,
        }
    }

    #[must_use]
    pub fn from_xy(x: f32, y: f32) -> Self {
        Self::new(Vec2::new(x, y), 0.0, 0.0)
    }

    #[must_use]
    pub fn with_elevation(mut self, elev_z: f32) -> Self {
        self.elev_z = sanitize_f32(elev_z, 0.0);
        self
    }

    #[must_use]
    pub fn with_heading(mut self, heading_rad: f32) -> Self {
        self.heading_rad = sanitize_f32(heading_rad, 0.0);
        self
    }

    #[must_use]
    pub fn with_pitch_roll(mut self, pitch_rad: f32, roll_rad: f32) -> Self {
        self.pitch_rad = sanitize_f32(pitch_rad, 0.0);
        self.roll_rad = sanitize_f32(roll_rad, 0.0);
        self
    }

    #[must_use]
    pub fn sanitized(self) -> Self {
        Self {
            pos: sanitize_vec2(self.pos, Vec2::ZERO),
            elev_z: sanitize_f32(self.elev_z, 0.0),
            heading_rad: sanitize_f32(self.heading_rad, 0.0),
            pitch_rad: sanitize_f32(self.pitch_rad, 0.0),
            roll_rad: sanitize_f32(self.roll_rad, 0.0),
        }
    }
}

/// Optional deterministic depth layer bias added when computing `iso_depth_key` for an entity.
/// Ground/background and particle overlay layers use large offsets, whereas mutually occluding
/// 3D world objects share `ISO_BIAS_WORLD_OBJECTS` (`10.0`) with tiny tie-break offsets (`<= 6e-4`)
/// so world-space isometric depth `-(ground_iso.y)` always governs occlusion.
#[derive(Component, Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct IsoLayerBias(pub f32);

/// Base terrain backdrop layer bias (`-550.0`).
pub const ISO_BIAS_GROUND_BACKDROP: f32 = -550.0;
/// Base chunked isometric tilemap layer bias (`-480.0`).
pub const ISO_BIAS_GROUND_TILES: f32 = -480.0;
/// Dynamic ground decal / scar layer bias (`-420.0`).
pub const ISO_BIAS_GROUND_DECALS: f32 = -420.0;
/// Building interior floor slab layer bias (`-360.0`), above ground decals and below 3D world objects.
pub const ISO_BIAS_INTERIOR_FLOOR: f32 = -360.0;
/// Common base layer bias (`10.0`) shared by all mutually occluding 3D world objects.
pub const ISO_BIAS_WORLD_OBJECTS: f32 = 10.0;
/// Tie-break bias for structural debris chunks (`10.0001`).
pub const ISO_BIAS_DEBRIS: f32 = ISO_BIAS_WORLD_OBJECTS + 0.0001;
/// Tie-break bias for interior furniture and exterior street props (`10.0002`).
pub const ISO_BIAS_PROP: f32 = ISO_BIAS_WORLD_OBJECTS + 0.0002;
/// Tie-break bias for pedestrians (`10.0003`).
pub const ISO_BIAS_PEDESTRIAN: f32 = ISO_BIAS_WORLD_OBJECTS + 0.0003;
/// Tie-break bias for traffic vehicles (`10.0004`).
pub const ISO_BIAS_VEHICLE: f32 = ISO_BIAS_WORLD_OBJECTS + 0.0004;
/// Tie-break bias for modular buildings (`10.0005`).
pub const ISO_BIAS_BUILDING: f32 = ISO_BIAS_WORLD_OBJECTS + 0.0005;
/// Tie-break bias for the player's armored bulldozer (`10.0006`).
pub const ISO_BIAS_DOZER: f32 = ISO_BIAS_WORLD_OBJECTS + 0.0006;
/// Airborne particle burst / weather overlay layer bias (`120.0`).
pub const ISO_BIAS_PARTICLES: f32 = 120.0;

/// Projects Cartesian 2D world coordinates `(world_xy)` and vertical elevation `elev_z` into
/// 2:1 isometric screen coordinates (`+X` world -> down-right `(2:1)`, `+Y` world -> down-left `(2:1)`,
/// `+Z` elevation -> straight up on screen).
#[must_use]
pub fn world_to_iso_screen(world_xy: Vec2, elev_z: f32) -> Vec2 {
    let safe_xy = sanitize_vec2(world_xy, Vec2::ZERO);
    let safe_z = sanitize_f32(elev_z, 0.0).clamp(-500.0, 2_000.0);
    let screen_x = safe_xy.x - safe_xy.y;
    let screen_y = -(safe_xy.x + safe_xy.y) * 0.5 + safe_z;
    sanitize_vec2(Vec2::new(screen_x, screen_y), Vec2::ZERO)
}

/// Inverse of `world_to_iso_screen`: converts isometric screen coordinates `screen_xy` at elevation
/// `elev_z` back into 2D Cartesian world coordinates.
#[must_use]
pub fn iso_screen_to_world(screen_xy: Vec2, elev_z: f32) -> Vec2 {
    let safe_s = sanitize_vec2(screen_xy, Vec2::ZERO);
    let safe_z = sanitize_f32(elev_z, 0.0).clamp(-500.0, 2_000.0);
    let adj_y = -(safe_s.y - safe_z) * 2.0;
    sanitize_vec2(
        Vec2::new((adj_y + safe_s.x) * 0.5, (adj_y - safe_s.x) * 0.5),
        Vec2::ZERO,
    )
}

/// Computes a deterministic painter's Z depth key in `[-800.0, 800.0]` from Cartesian `world_xy`,
/// vertical `elev_z`, and `layer_bias`. Positional isometric depth (`-ground_iso.y * 0.05`) spans
/// `~50.0` units across the playable site so world position determines occlusion among objects
/// sharing `ISO_BIAS_WORLD_OBJECTS`, while `layer_bias` separates ground/overlay layers and breaks
/// exact ties.
#[must_use]
pub fn iso_depth_key(world_xy: Vec2, elev_z: f32, layer_bias: f32) -> f32 {
    let safe_xy = sanitize_vec2(world_xy, Vec2::ZERO);
    let safe_z = sanitize_f32(elev_z, 0.0).clamp(0.0, 500.0);
    let bias = sanitize_f32(layer_bias, 0.0);
    let ground_iso = world_to_iso_screen(safe_xy, 0.0);
    let raw_z = bias - (ground_iso.y * 0.05) - (ground_iso.x * 0.00001) + (safe_z * 0.002);
    sanitize_f32(raw_z, 0.0).clamp(-800.0, 800.0)
}

/// Offsets a 2D Cartesian world point by `along` (along `heading_rad`) and `across`
/// (perpendicular right of `heading_rad`), matching `headingOffset` in `drawIso.ts`.
#[must_use]
pub fn heading_offset(origin: Vec2, heading_rad: f32, along: f32, across: f32) -> Vec2 {
    let p = sanitize_vec2(origin, Vec2::ZERO);
    let h = sanitize_f32(heading_rad, 0.0);
    let a = sanitize_f32(along, 0.0);
    let c = sanitize_f32(across, 0.0);
    let (fy, fx) = h.sin_cos();
    sanitize_vec2(Vec2::new(p.x + fx * a - fy * c, p.y + fy * a + fx * c), p)
}

/// Optional roof/wedge slope profile for a 2.5D extruded isometric box (`IsoExtrudedBox`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum RoofSlopeSpec {
    /// Linearly slopes across the width (`+width/2` to `-width/2`), matching `slope = true` in `drawOrientedIsoBox`.
    AcrossWidth { low_scale: f32, high_scale: f32 },
    /// Linearly slopes along the length (`-length/2` to `+length/2`).
    AlongLength { back_scale: f32, front_scale: f32 },
    /// Pitched gable roof with a raised ridge along the centerline.
    Gable { eave_scale: f32, ridge_scale: f32 },
}

/// 2.5D extruded isometric box specification (`drawOrientedIsoBox`) rendered with upper-west
/// sun lighting on its visible side faces and top lid plus an optional ground drop-shadow.
#[derive(Component, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct IsoExtrudedBox {
    pub center: Vec2,
    pub heading: f32,
    pub pitch: f32,
    pub roll: f32,
    pub length: f32,
    pub width: f32,
    pub z0: f32,
    pub height: f32,
    pub base_color: Color,
    pub alpha: f32,
    pub roof_slope: Option<RoofSlopeSpec>,
    pub cast_shadow: bool,
}

impl Default for IsoExtrudedBox {
    fn default() -> Self {
        Self {
            center: Vec2::ZERO,
            heading: 0.0,
            pitch: 0.0,
            roll: 0.0,
            length: 16.0,
            width: 16.0,
            z0: 0.0,
            height: 12.0,
            base_color: Color::srgb(0.72, 0.72, 0.74),
            alpha: 1.0,
            roof_slope: None,
            cast_shadow: true,
        }
    }
}

impl IsoExtrudedBox {
    #[must_use]
    pub fn new(length: f32, width: f32, height: f32, base_color: Color) -> Self {
        Self {
            center: Vec2::ZERO,
            heading: 0.0,
            pitch: 0.0,
            roll: 0.0,
            length: sanitize_f32(length, 1.0).max(0.1),
            width: sanitize_f32(width, 1.0).max(0.1),
            z0: 0.0,
            height: sanitize_f32(height, 1.0).max(0.05),
            base_color,
            alpha: 1.0,
            roof_slope: None,
            cast_shadow: false,
        }
    }

    #[must_use]
    pub fn with_offset(mut self, center: Vec2, z0: f32) -> Self {
        self.center = sanitize_vec2(center, Vec2::ZERO);
        self.z0 = sanitize_f32(z0, 0.0).max(0.0);
        self
    }

    #[must_use]
    pub fn with_heading(mut self, heading: f32) -> Self {
        self.heading = sanitize_f32(heading, 0.0);
        self
    }

    #[must_use]
    pub fn with_shadow(mut self, cast_shadow: bool) -> Self {
        self.cast_shadow = cast_shadow;
        self
    }

    #[must_use]
    pub fn with_alpha(mut self, alpha: f32) -> Self {
        self.alpha = sanitize_f32(alpha, 1.0).clamp(0.0, 1.0);
        self
    }

    #[must_use]
    pub fn with_pitch_roll(mut self, pitch: f32, roll: f32) -> Self {
        self.pitch = sanitize_f32(pitch, 0.0);
        self.roll = sanitize_f32(roll, 0.0);
        self
    }

    #[must_use]
    pub fn with_roof_slope(mut self, slope: RoofSlopeSpec) -> Self {
        self.roof_slope = Some(slope);
        self
    }
}

/// Multi-box 2.5D isometric volume assembly attached to an entity (dozer, building, prop,
/// vehicle, pedestrian, or debris cluster).
#[derive(Component, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IsoBoxAssembly {
    pub parts: Vec<IsoExtrudedBox>,
    pub alpha_multiplier: f32,
}

impl Default for IsoBoxAssembly {
    fn default() -> Self {
        Self {
            parts: Vec::new(),
            alpha_multiplier: 1.0,
        }
    }
}

impl IsoBoxAssembly {
    #[must_use]
    pub fn new(parts: Vec<IsoExtrudedBox>) -> Self {
        Self {
            parts,
            alpha_multiplier: 1.0,
        }
    }
}

/// Resource tracking the 6-digit demolition cash counter and smooth odometer reel state.
#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize)]
pub struct CashLedger {
    pub current_cash: u32,
    pub lifetime_cash: u64,
    /// Smoothly interpolated value (`0.0..=999_999.0`) used to drive the 6 physical odometer reels.
    pub odometer_display_value: f32,
}

impl CashLedger {
    pub fn reset(&mut self) {
        self.current_cash = 0;
        self.lifetime_cash = 0;
        self.odometer_display_value = 0.0;
    }

    /// Computes individual reel positions (`[0.0..10.0); 6]`) from most-significant (`$X00,000`)
    /// to least-significant (`$000,00X`) for the rolling mechanical odometer HUD.
    #[must_use]
    pub fn odometer_reel_digits(&self) -> [f32; 6] {
        let clamped = sanitize_f32(self.odometer_display_value, 0.0)
            .clamp(0.0, MAX_ODOMETER_CASH as f32);
        let whole = clamped.floor() as u32;
        let frac = clamped - (whole as f32);

        let d0 = (whole % 10) as f32 + frac;
        let d1 = ((whole / 10) % 10) as f32 + if whole % 10 == 9 { frac } else { 0.0 };
        let d2 = ((whole / 100) % 10) as f32 + if whole % 100 == 99 { frac } else { 0.0 };
        let d3 = ((whole / 1_000) % 10) as f32 + if whole % 1_000 == 999 { frac } else { 0.0 };
        let d4 = ((whole / 10_000) % 10) as f32 + if whole % 10_000 == 9_999 { frac } else { 0.0 };
        let d5 = ((whole / 100_000) % 10) as f32
            + if whole % 100_000 == 99_999 { frac } else { 0.0 };

        [d5, d4, d3, d2, d1, d0]
    }
}

/// Single pre-allocated slot in the floating world-space cash popup ring buffer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CashPopupSlot {
    pub amount: u32,
    pub world_pos: Vec3,
    pub spawn_time_secs: f32,
    pub remaining_lifetime: f32,
    pub active: bool,
}

impl Default for CashPopupSlot {
    fn default() -> Self {
        Self {
            amount: 0,
            world_pos: Vec3::ZERO,
            spawn_time_secs: 0.0,
            remaining_lifetime: 0.0,
            active: false,
        }
    }
}

/// Fixed-capacity ring-buffer pool for floating yellow world-space cash popups (`+$5`, `+$89`).
#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
pub struct CashPopupPool {
    pub slots: Vec<CashPopupSlot>,
    pub current_time_secs: f32,
    pub fallback_dozer_pos: Vec3,
}

impl Default for CashPopupPool {
    fn default() -> Self {
        Self {
            slots: vec![CashPopupSlot::default(); MAX_CASH_POPUPS],
            current_time_secs: 0.0,
            fallback_dozer_pos: Vec3::ZERO,
        }
    }
}

impl CashPopupPool {
    pub fn reset(&mut self) {
        for slot in &mut self.slots {
            *slot = CashPopupSlot::default();
        }
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.slots.iter().filter(|s| s.active).count()
    }
}

/// Contract Function (Module A): Validates and adds demolition cash (`+$5`, `+$71`, `+$89`)
/// to the player's total, clamping at the 6-digit odometer ceiling (`$999,999`), and requests
/// or coalesces a floating world-space UI popup in the pre-allocated pool.
pub fn try_award_demolition_cash(
    ledger: &mut CashLedger,
    popup_pool: &mut CashPopupPool,
    amount: u32,
    world_pos: Vec3,
) -> Result<u32, LedgerError> {
    if popup_pool.slots.is_empty() {
        return Err(LedgerError::PoolCorrupted);
    }

    // 1. Check if amount == 0; if so, return early with current balance (no-op).
    if amount == 0 {
        return Ok(ledger.current_cash);
    }

    // 2. Saturating addition clamped to the 6-digit odometer ceiling ($999,999).
    ledger.current_cash = ledger
        .current_cash
        .saturating_add(amount)
        .min(MAX_ODOMETER_CASH);
    ledger.lifetime_cash = ledger.lifetime_cash.saturating_add(u64::from(amount));

    // 3. Validate world_pos is finite; if invalid, fallback to player dozer position.
    let safe_pos = sanitize_vec3(world_pos, popup_pool.fallback_dozer_pos);
    let now = sanitize_f32(popup_pool.current_time_secs, 0.0);

    // 4a. Spatial/Temporal Coalescing: coalesce events within 100ms window and 3.0m radius.
    let coalesce_radius_sq = COALESCE_RADIUS_METERS * COALESCE_RADIUS_METERS;
    for slot in &mut popup_pool.slots {
        if slot.active {
            let dt_spawn = (now - slot.spawn_time_secs).abs();
            let dist_sq = slot.world_pos.distance_squared(safe_pos);
            if dt_spawn <= COALESCE_WINDOW_SECS && dist_sq <= coalesce_radius_sq {
                slot.amount = slot.amount.saturating_add(amount).min(MAX_ODOMETER_CASH);
                slot.remaining_lifetime = DEFAULT_POPUP_LIFETIME_SECS;
                return Ok(ledger.current_cash);
            }
        }
    }

    // 4b. Allocate an inactive slot if available.
    if let Some(free_slot) = popup_pool.slots.iter_mut().find(|s| !s.active) {
        *free_slot = CashPopupSlot {
            amount: amount.min(MAX_ODOMETER_CASH),
            world_pos: safe_pos,
            spawn_time_secs: now,
            remaining_lifetime: DEFAULT_POPUP_LIFETIME_SECS,
            active: true,
        };
        return Ok(ledger.current_cash);
    }

    // 4c. Pool is full (50 active popups): coalesce into nearest active popup (preferring within 2m).
    let mut nearest_idx: usize = 0;
    let mut nearest_dist_sq: f32 = f32::MAX;
    for (idx, slot) in popup_pool.slots.iter().enumerate() {
        let dist_sq = slot.world_pos.distance_squared(safe_pos);
        if dist_sq < nearest_dist_sq {
            nearest_dist_sq = dist_sq;
            nearest_idx = idx;
        }
    }

    let _within_two_meters =
        nearest_dist_sq <= FULL_POOL_COALESCE_RADIUS_METERS * FULL_POOL_COALESCE_RADIUS_METERS;
    if let Some(target_slot) = popup_pool.slots.get_mut(nearest_idx) {
        target_slot.amount = target_slot
            .amount
            .saturating_add(amount)
            .min(MAX_ODOMETER_CASH);
        target_slot.remaining_lifetime = DEFAULT_POPUP_LIFETIME_SECS;
    }

    Ok(ledger.current_cash)
}

/// Digital 7-segment elapsed timer (`MM:SS`) resource.
#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize)]
pub struct ElapsedClock {
    pub elapsed_secs: f32,
}

impl ElapsedClock {
    pub fn reset(&mut self) {
        self.elapsed_secs = 0.0;
    }

    pub fn tick(&mut self, dt: f32) {
        if let Ok(safe_dt) = sanitize_dt(dt) {
            self.elapsed_secs = sanitize_f32(self.elapsed_secs + safe_dt, self.elapsed_secs);
        }
    }

    /// Formats the elapsed timer as `MM:SS` (clamped at `99:59`).
    #[must_use]
    pub fn format_mm_ss(&self) -> String {
        let total_secs = (sanitize_f32(self.elapsed_secs, 0.0).max(0.0) as u32).min(5999);
        let minutes = total_secs / 60;
        let seconds = total_secs % 60;
        format!("{minutes:02}:{seconds:02}")
    }
}

/// System ticking the elapsed clock, cash popup lifetimes, and odometer reel interpolation.
pub fn tick_core_economy_and_clock_system(
    time: Res<Time>,
    mut clock: ResMut<ElapsedClock>,
    mut ledger: ResMut<CashLedger>,
    mut popup_pool: ResMut<CashPopupPool>,
    mut cash_events: EventReader<DemolitionCashEvent>,
) {
    let raw_dt = time.delta_secs();
    let Ok(dt) = sanitize_dt(raw_dt) else {
        return;
    };

    clock.tick(dt);
    popup_pool.current_time_secs = clock.elapsed_secs;

    for ev in cash_events.read() {
        if let Err(err) =
            try_award_demolition_cash(&mut ledger, &mut popup_pool, ev.amount, ev.world_pos)
        {
            warn!("Failed to award demolition cash: {err:?}");
        }
    }

    // Advance floating popups upward and expire completed slots.
    for slot in &mut popup_pool.slots {
        if slot.active {
            slot.remaining_lifetime -= dt;
            slot.world_pos.y = sanitize_f32(slot.world_pos.y + 18.0 * dt, slot.world_pos.y);
            if slot.remaining_lifetime <= 0.0 {
                slot.active = false;
            }
        }
    }

    // Smoothly roll odometer display value toward target cash balance.
    let target = ledger.current_cash as f32;
    let diff = target - ledger.odometer_display_value;
    if diff.abs() < 0.05 {
        ledger.odometer_display_value = target;
    } else {
        let step = diff * (12.0 * dt).clamp(0.0, 1.0);
        ledger.odometer_display_value =
            sanitize_f32(ledger.odometer_display_value + step, target).clamp(0.0, MAX_ODOMETER_CASH as f32);
    }
}

/// Handles `SiteRestartRequested` by incrementing `SessionEpoch`, resetting economy/timer state,
/// and despawning all `SiteScoped` entities deterministically.
pub fn handle_site_restart_system(
    mut commands: Commands,
    mut restart_events: EventReader<SiteRestartRequested>,
    mut epoch: ResMut<SessionEpoch>,
    mut ledger: ResMut<CashLedger>,
    mut popup_pool: ResMut<CashPopupPool>,
    mut clock: ResMut<ElapsedClock>,
    mut env_config: ResMut<EnvironmentConfig>,
    mut collapse_zones: ResMut<ActiveCollapseZones>,
    site_entities: Query<Entity, With<SiteScoped>>,
) {
    let mut should_restart = false;
    for _ in restart_events.read() {
        should_restart = true;
    }
    if !should_restart {
        return;
    }

    let new_epoch = epoch.advance();
    ledger.reset();
    popup_pool.reset();
    clock.reset();
    collapse_zones.zones.clear();
    env_config.resolved_season = env_config.season.resolve(new_epoch.0);

    for entity in &site_entities {
        if let Some(entity_commands) = commands.get_entity(entity) {
            entity_commands.despawn_recursive();
        }
    }
}

/// Core Bevy plugin registering states, epoch counters, cash ledger, and elapsed clock.
pub struct PdCorePlugin;

impl Plugin for PdCorePlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<AppState>()
            .init_resource::<EnvironmentConfig>()
            .init_resource::<SessionEpoch>()
            .init_resource::<CashLedger>()
            .init_resource::<CashPopupPool>()
            .init_resource::<ElapsedClock>()
            .init_resource::<ActiveCollapseZones>()
            .insert_resource(DebugTelemetry {
                sound_enabled: true,
                ..Default::default()
            })
            .add_event::<SiteRestartRequested>()
            .add_event::<DemolitionCashEvent>()
            .add_event::<DecalSpawnRequest>()
            .add_event::<ParticleBurstEvent>()
            .add_event::<AudioTriggerEvent>()
            .configure_sets(
                Update,
                (
                    PdSystemSet::InputAndClock,
                    PdSystemSet::WorldTilemapSync,
                    PdSystemSet::HudInterpolation,
                )
                    .chain(),
            )
            .configure_sets(
                FixedUpdate,
                (
                    PdSystemSet::DozerKinematics,
                    PdSystemSet::DestructionGraph,
                    PdSystemSet::DebrisLifecycle,
                    PdSystemSet::CrowdAndTrafficAi,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    handle_site_restart_system,
                    tick_core_economy_and_clock_system
                        .run_if(in_state(AppState::Playing)),
                )
                    .chain()
                    .in_set(PdSystemSet::InputAndClock),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ledger_zero_and_saturating_extremes() {
        let mut ledger = CashLedger::default();
        let mut pool = CashPopupPool::default();

        // amount == 0 is a no-op
        let res0 = try_award_demolition_cash(&mut ledger, &mut pool, 0, Vec3::ZERO);
        assert_eq!(res0, Ok(0));
        assert_eq!(pool.active_count(), 0);

        // Non-finite position falls back cleanly and saturates at $999,999
        pool.fallback_dozer_pos = Vec3::new(12.0, 34.0, 0.0);
        let res_max = try_award_demolition_cash(
            &mut ledger,
            &mut pool,
            u32::MAX,
            Vec3::new(f32::NAN, f32::INFINITY, f32::NEG_INFINITY),
        );
        assert_eq!(res_max, Ok(MAX_ODOMETER_CASH));
        assert_eq!(pool.active_count(), 1);
        let first = pool.slots.first().copied().unwrap_or_default();
        assert!(first.world_pos.is_finite());
        assert_eq!(first.amount, MAX_ODOMETER_CASH);
    }

    #[test]
    fn test_massive_collapse_1000_cash_events_coalesce_within_pool_cap() {
        let mut ledger = CashLedger::default();
        let mut pool = CashPopupPool::default();

        // Simulate 1,200 cash events in a single frame across diverse positions
        for i in 0..1_200_u32 {
            let pos = Vec3::new((i as f32) * 0.5, (i as f32) * 0.25, 0.0);
            let res = try_award_demolition_cash(&mut ledger, &mut pool, 25, pos);
            assert!(res.is_ok());
        }

        assert!(pool.active_count() <= MAX_CASH_POPUPS);
        assert_eq!(ledger.current_cash, 30_000);
    }

    #[test]
    fn test_world_pose_and_isometric_projection_axes() {
        // +X in world space moves diagonally down-right (2:1) on screen
        let plus_x = world_to_iso_screen(Vec2::new(20.0, 0.0), 0.0);
        assert!(plus_x.x > 0.0);
        assert!(plus_x.y < 0.0);
        assert!((plus_x.x.abs() - 2.0 * plus_x.y.abs()).abs() < 1e-4);

        // +Y in world space moves diagonally down-left (2:1) on screen
        let plus_y = world_to_iso_screen(Vec2::new(0.0, 20.0), 0.0);
        assert!(plus_y.x < 0.0);
        assert!(plus_y.y < 0.0);
        assert!((plus_y.x.abs() - 2.0 * plus_y.y.abs()).abs() < 1e-4);

        // Round-trip screen -> world
        let orig = Vec2::new(42.5, -18.25);
        let screen = world_to_iso_screen(orig, 12.0);
        let recovered = iso_screen_to_world(screen, 12.0);
        assert!((recovered - orig).length() < 1e-3);

        // Non-finite inputs sanitize cleanly without panicking
        let nan_pose = WorldPose {
            pos: Vec2::new(f32::NAN, f32::INFINITY),
            elev_z: f32::NAN,
            heading_rad: f32::INFINITY,
            pitch_rad: f32::NEG_INFINITY,
            roll_rad: f32::NAN,
        }
        .sanitized();
        assert!(nan_pose.pos.is_finite());
        assert!(nan_pose.elev_z.is_finite());
        assert!(nan_pose.heading_rad.is_finite());
        assert!(nan_pose.pitch_rad.is_finite());
        assert!(nan_pose.roll_rad.is_finite());
        assert!(iso_depth_key(Vec2::splat(f32::NAN), f32::INFINITY, f32::NAN).is_finite());

        // Cross-category occlusion: a building closer to the camera (higher x + y) must have a
        // greater Z depth key than a dozer behind it, and vice versa when the dozer moves in front.
        let building_pos = Vec2::new(0.0, 120.0);
        let dozer_behind_pos = Vec2::new(0.0, 40.0);
        let dozer_in_front_pos = Vec2::new(0.0, 180.0);
        let building_z = iso_depth_key(building_pos, 0.0, ISO_BIAS_BUILDING);
        let dozer_behind_z = iso_depth_key(dozer_behind_pos, 0.0, ISO_BIAS_DOZER);
        let dozer_front_z = iso_depth_key(dozer_in_front_pos, 0.0, ISO_BIAS_DOZER);
        assert!(building_z > dozer_behind_z);
        assert!(dozer_front_z > building_z);
    }
}
