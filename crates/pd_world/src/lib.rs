//! `pd_world`: 2.5D isometric tilemap pipeline (`bevy_ecs_tilemap`), procedural 64-tile texture
//! atlas (`512x512`), bitmask road auto-tiling, seasonal palette swaps, dynamic ground decal
//! ring buffer, pooled weather/burst particle system, interior cutaway masks, and spatial hash grid.
//!
//! Depends only on `pd_core`.

use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy_ecs_tilemap::prelude::*;
use pd_core::{
    AppState, CampaignLevel, DecalKind, DecalSpawnRequest, EnvironmentConfig, ParticleBurstEvent,
    ParticleBurstKind, PdSystemSet, Season, SessionEpoch, SiteRestartRequested, SiteScoped,
    WeatherSetting, sanitize_dt, sanitize_f32, sanitize_vec2,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Fallback tile/sprite index used whenever an out-of-bounds atlas index is requested.
pub const FALLBACK_ERROR_TILE_INDEX: u32 = 0;

/// Maximum valid tile index in the 8x8 procedural terrain/structural atlas (64 tiles: 0..=63).
pub const MAX_PROCEDURAL_ATLAS_INDEX: u32 = 63;

/// Maximum persistent ground decals before deterministic ring-buffer eviction.
pub const MAX_GROUND_DECALS: usize = 300;

/// Maximum simultaneous weather and burst particles in the world particle ring buffer.
pub const MAX_WORLD_PARTICLES: usize = 160;

/// Isometric tile width in pixels.
pub const ISO_TILE_WIDTH: f32 = 64.0;

/// Isometric tile height in pixels.
pub const ISO_TILE_HEIGHT: f32 = 32.0;

/// Default spatial hash cell size (world units).
pub const SPATIAL_CELL_SIZE: f32 = 48.0;

/// Named atlas tile indices inside the 8x8 (`64x64` cell) procedural sprite sheet.
pub mod atlas_indices {
    /// Road auto-tiles occupy indices `0..=15` corresponding directly to 4-bit `N|E|S|W` bitmask.
    pub const ROAD_BITMASK_BASE: u32 = 0;
    pub const TERRAIN_GRASS: u32 = 16;
    pub const TERRAIN_DIRT: u32 = 17;
    pub const TERRAIN_GRAVEL: u32 = 18;
    pub const TERRAIN_CONCRETE_FOUNDATION: u32 = 19;
    pub const TERRAIN_SIDEWALK: u32 = 20;
    pub const TERRAIN_RUBBLE_SCAR: u32 = 21;
    pub const WALL_INTACT: u32 = 24;
    pub const WALL_CRACKED: u32 = 25;
    pub const WALL_RUBBLE: u32 = 26;
    pub const PILLAR_INTACT: u32 = 27;
    pub const PILLAR_CRACKED: u32 = 28;
    pub const PILLAR_RUBBLE: u32 = 29;
    pub const FLOOR_CHECKER: u32 = 30;
    pub const ROOF_CORRUGATED: u32 = 31;
    pub const PROP_CRATE_OR_FURNITURE: u32 = 32;
    pub const PROP_EXPLOSIVE_TANK: u32 = 33;
    pub const PROP_TREE_FOLIAGE: u32 = 34;
}

/// Terrain and road tile categories.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum TerrainTileKind {
    #[default]
    Grass,
    Dirt,
    Gravel,
    ConcreteFoundation,
    RoadTwoLane,
    RoadFourLane,
    Sidewalk,
    RubbleScar,
}

/// Seasonal color palette for ground cover and foliage.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SeasonalPalette {
    pub grass_tint: Color,
    pub foliage_primary: Color,
    pub foliage_secondary: Color,
    pub ambient_fog_alpha: f32,
}

impl SeasonalPalette {
    #[must_use]
    pub const fn for_season(season: Season) -> Self {
        match season {
            Season::Spring | Season::Random => Self {
                grass_tint: Color::srgb(0.36, 0.62, 0.29),
                foliage_primary: Color::srgb(0.42, 0.72, 0.34),
                foliage_secondary: Color::srgb(0.88, 0.64, 0.76),
                ambient_fog_alpha: 0.04,
            },
            Season::Summer => Self {
                grass_tint: Color::srgb(0.28, 0.54, 0.22),
                foliage_primary: Color::srgb(0.24, 0.58, 0.21),
                foliage_secondary: Color::srgb(0.32, 0.66, 0.26),
                ambient_fog_alpha: 0.02,
            },
            Season::Autumn => Self {
                grass_tint: Color::srgb(0.48, 0.46, 0.24),
                foliage_primary: Color::srgb(0.84, 0.42, 0.14),
                foliage_secondary: Color::srgb(0.76, 0.22, 0.16),
                ambient_fog_alpha: 0.07,
            },
            Season::Winter => Self {
                grass_tint: Color::srgb(0.84, 0.88, 0.92),
                foliage_primary: Color::srgb(0.78, 0.83, 0.88),
                foliage_secondary: Color::srgb(0.52, 0.56, 0.60),
                ambient_fog_alpha: 0.12,
            },
        }
    }
}

/// Validates a texture atlas index against `max_index`, returning `FALLBACK_ERROR_TILE_INDEX` (0)
/// and logging a structured warning if out of range.
#[must_use]
pub fn validate_atlas_index(index: u32, max_index: u32) -> u32 {
    if index <= max_index {
        index
    } else {
        warn!(
            "Out-of-bounds texture atlas index {} (max {}); using fallback index {}.",
            index, max_index, FALLBACK_ERROR_TILE_INDEX
        );
        FALLBACK_ERROR_TILE_INDEX
    }
}

/// Validates 2D grid coordinates against `(width, height)` bounds without panicking.
#[must_use]
pub fn validate_grid_coord(x: i32, y: i32, width: u32, height: u32) -> Option<(u32, u32)> {
    let ux = u32::try_from(x).ok()?;
    let uy = u32::try_from(y).ok()?;
    if ux < width && uy < height {
        Some((ux, uy))
    } else {
        None
    }
}

/// Computes a 4-bit cardinal road connectivity bitmask (`N=1, E=2, S=4, W=8`).
#[must_use]
pub const fn compute_road_bitmask(north: bool, east: bool, south: bool, west: bool) -> u8 {
    (if north { 1 } else { 0 })
        | (if east { 2 } else { 0 })
        | (if south { 4 } else { 0 })
        | (if west { 8 } else { 0 })
}

/// Converts 2.5D grid coordinates `(grid_x, grid_y)` and simulated vertical `z_height` into
/// isometric 2D world space with stable Z-ordering (preventing sprite Z-fighting).
#[must_use]
pub fn grid_to_iso_world(grid_x: i32, grid_y: i32, simulated_height: f32) -> Vec3 {
    let gx = grid_x as f32;
    let gy = grid_y as f32;
    let safe_h = sanitize_f32(simulated_height, 0.0).clamp(0.0, 500.0);

    let iso_x = (gx - gy) * (ISO_TILE_WIDTH * 0.5);
    let iso_y = (gx + gy) * (ISO_TILE_HEIGHT * 0.5) + safe_h;
    let stable_z = compute_stable_iso_z(iso_x, iso_y - safe_h, safe_h, 0.0);

    Vec3::new(
        sanitize_f32(iso_x, 0.0),
        sanitize_f32(iso_y, 0.0),
        stable_z,
    )
}

/// Computes a deterministic Z depth within `[-800.0, 800.0]` to prevent sprite Z-fighting.
#[must_use]
pub fn compute_stable_iso_z(
    world_x: f32,
    ground_y: f32,
    simulated_height: f32,
    layer_bias: f32,
) -> f32 {
    let sx = sanitize_f32(world_x, 0.0);
    let sy = sanitize_f32(ground_y, 0.0);
    let sh = sanitize_f32(simulated_height, 0.0).clamp(0.0, 500.0);
    let bias = sanitize_f32(layer_bias, 0.0);

    let raw_z = bias - (sy * 0.001) - (sx * 0.00001) + (sh * 0.01);
    sanitize_f32(raw_z, 0.0).clamp(-800.0, 800.0)
}

/// Interior cutaway mask component for building roofs and upper stories.
#[derive(Component, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CutawayFootprint {
    pub min_world: Vec2,
    pub max_world: Vec2,
    pub exterior_breached: bool,
    pub current_roof_alpha: f32,
}

impl CutawayFootprint {
    #[must_use]
    pub fn contains_point(&self, point: Vec2) -> bool {
        let p = sanitize_vec2(point, Vec2::splat(f32::MAX));
        p.x >= self.min_world.x
            && p.x <= self.max_world.x
            && p.y >= self.min_world.y
            && p.y <= self.max_world.y
    }

    /// Updates roof cutaway alpha smoothly in `[0.15, 1.0]` based on breach status or dozer entry.
    pub fn update_alpha(&mut self, dozer_pos: Vec2, dt: f32) {
        let safe_dt = sanitize_f32(dt, 0.016).clamp(0.0, 0.1);
        let target_alpha = if self.exterior_breached || self.contains_point(dozer_pos) {
            0.15_f32
        } else {
            1.0_f32
        };
        let step = (target_alpha - self.current_roof_alpha) * (8.0 * safe_dt).clamp(0.0, 1.0);
        self.current_roof_alpha =
            sanitize_f32(self.current_roof_alpha + step, target_alpha).clamp(0.15, 1.0);
    }
}

/// Single node on the road spline graph used by ambient and emergency vehicles.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RoadLaneNode {
    pub position: Vec2,
    pub next_node: Option<usize>,
    pub prev_node: Option<usize>,
    pub blocked_by_rubble: bool,
}

/// Resource storing the road lane spline graph for the current site.
#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoadSplineGraph {
    pub nodes: Vec<RoadLaneNode>,
}

impl RoadSplineGraph {
    /// Builds a clean perimeter + cross-street road spline loop for the given campaign level.
    pub fn rebuild_for_level(&mut self, level: CampaignLevel) {
        self.nodes.clear();
        let half_span = 260.0 + f32::from(level.index_one_based()) * 32.0;
        let waypoints = [
            Vec2::new(-half_span, -half_span * 0.65),
            Vec2::new(0.0, -half_span * 0.65),
            Vec2::new(half_span, -half_span * 0.65),
            Vec2::new(half_span, 0.0),
            Vec2::new(half_span, half_span * 0.65),
            Vec2::new(0.0, half_span * 0.65),
            Vec2::new(-half_span, half_span * 0.65),
            Vec2::new(-half_span, 0.0),
        ];

        let count = waypoints.len();
        for (i, &pos) in waypoints.iter().enumerate() {
            self.nodes.push(RoadLaneNode {
                position: pos,
                next_node: Some((i + 1) % count),
                prev_node: Some(if i == 0 { count - 1 } else { i - 1 }),
                blocked_by_rubble: false,
            });
        }
    }

    /// Flags any road spline node within `radius` of `world_pos` as obstructed by rubble.
    pub fn mark_rubble_obstruction(&mut self, world_pos: Vec2, radius: f32) {
        let safe_pos = sanitize_vec2(world_pos, Vec2::ZERO);
        let r_sq = sanitize_f32(radius, 0.0).max(0.0).powi(2);
        for node in &mut self.nodes {
            if node.position.distance_squared(safe_pos) <= r_sq {
                node.blocked_by_rubble = true;
            }
        }
    }
}

/// Spatial hash grid partitioning agents (pedestrians and vehicles) into uniform 2D cells
/// for bounded $O(1)$ neighborhood lookups.
#[derive(Resource, Debug, Clone)]
pub struct SpatialHashGrid {
    pub cell_size: f32,
    pub cells: HashMap<(i32, i32), Vec<(Entity, Vec2)>>,
}

impl Default for SpatialHashGrid {
    fn default() -> Self {
        Self {
            cell_size: SPATIAL_CELL_SIZE,
            cells: HashMap::with_capacity(128),
        }
    }
}

impl SpatialHashGrid {
    pub fn clear(&mut self) {
        for bucket in self.cells.values_mut() {
            bucket.clear();
        }
    }

    #[must_use]
    pub fn cell_coord(&self, pos: Vec2) -> (i32, i32) {
        let safe_pos = sanitize_vec2(pos, Vec2::ZERO);
        let inv = 1.0 / self.cell_size.max(1.0);
        let cx = (safe_pos.x * inv).floor().clamp(-10_000.0, 10_000.0) as i32;
        let cy = (safe_pos.y * inv).floor().clamp(-10_000.0, 10_000.0) as i32;
        (cx, cy)
    }

    pub fn insert(&mut self, entity: Entity, pos: Vec2) {
        let safe_pos = sanitize_vec2(pos, Vec2::ZERO);
        let key = self.cell_coord(safe_pos);
        self.cells.entry(key).or_default().push((entity, safe_pos));
    }

    /// Populates `out_entities` with all entities within `radius` of `center`, without
    /// allocating a new `Vec` per call.
    pub fn query_radius(
        &self,
        center: Vec2,
        radius: f32,
        out_entities: &mut Vec<(Entity, Vec2)>,
    ) {
        let safe_center = sanitize_vec2(center, Vec2::ZERO);
        let safe_radius = sanitize_f32(radius, 0.0).clamp(0.0, 1_000.0);
        let radius_sq = safe_radius * safe_radius;

        let min_cell = self.cell_coord(safe_center - Vec2::splat(safe_radius));
        let max_cell = self.cell_coord(safe_center + Vec2::splat(safe_radius));

        for cy in min_cell.1..=max_cell.1 {
            for cx in min_cell.0..=max_cell.0 {
                if let Some(bucket) = self.cells.get(&(cx, cy)) {
                    for &(entity, pos) in bucket {
                        if pos.distance_squared(safe_center) <= radius_sq {
                            out_entities.push((entity, pos));
                        }
                    }
                }
            }
        }
    }
}

/// Resource holding the procedurally generated cohesive 2.5D tile & module texture sheet (`512x512`,
/// 64 tiles of `64x64`) and its `TextureAtlasLayout`.
#[derive(Resource, Debug, Clone, Default)]
pub struct ProceduralWorldAtlas {
    pub texture: Option<Handle<Image>>,
    pub layout: Option<Handle<TextureAtlasLayout>>,
}

/// Synthesizes a 512x512 RGBA sprite sheet (8x8 grid of 64x64 tiles) containing road auto-tiles
/// with yellow dashed centerlines & crosswalks, terrain tiles, and intact/cracked/rubble building modules.
#[must_use]
pub fn generate_cohesive_atlas_rgba_bytes() -> Vec<u8> {
    let sheet_w = 512_usize;
    let sheet_h = 512_usize;
    let tile_size = 64_usize;
    let mut pixels = vec![255_u8; sheet_w * sheet_h * 4];

    for tile_idx in 0..64_u32 {
        let col = (tile_idx as usize) % 8;
        let row = (tile_idx as usize) / 8;
        let ox = col * tile_size;
        let oy = row * tile_size;

        for ly in 0..tile_size {
            for lx in 0..tile_size {
                let gx = ox + lx;
                let gy = oy + ly;
                let base = (gy * sheet_w + gx) * 4;

                let is_ao_border = lx < 2 || ly < 2 || lx >= tile_size - 2 || ly >= tile_size - 2;
                let (mut r, mut g, mut b) = match tile_idx {
                    // 0..=15: Bitmask road auto-tiles (asphalt + yellow centerlines + crosswalks)
                    0..=15 => {
                        let mask = tile_idx as u8;
                        let mut cr = 54_u8;
                        let mut cg = 57_u8;
                        let mut cb = 62_u8;

                        // Sidewalk curb along unconnected edges
                        if (lx < 6 || lx >= tile_size - 6 || ly < 6 || ly >= tile_size - 6)
                            && mask != 15
                        {
                            cr = 148;
                            cg = 150;
                            cb = 146;
                        }
                        // Yellow dashed centerlines along connected axes
                        let center_x = (lx as i32 - 32).abs() <= 1;
                        let center_y = (ly as i32 - 32).abs() <= 1;
                        let dash_x = (lx / 6) % 2 == 0;
                        let dash_y = (ly / 6) % 2 == 0;
                        if ((mask & 0b0101 != 0) && center_x && dash_y)
                            || ((mask & 0b1010 != 0) && center_y && dash_x)
                        {
                            cr = 238;
                            cg = 192;
                            cb = 36;
                        }
                        // White crosswalk bars on 4-way intersection (mask == 15)
                        if mask == 15
                            && (lx < 10 || lx >= tile_size - 10)
                            && (ly % 8 < 4)
                        {
                            cr = 235;
                            cg = 235;
                            cb = 235;
                        }
                        (cr, cg, cb)
                    }
                    // 16: Grass with subtle blade noise
                    16 => {
                        let n = ((lx * 13 + ly * 7) % 16) as u8;
                        (88_u8.saturating_add(n), 152_u8.saturating_add(n), 72_u8)
                    }
                    // 17: Dirt / Mud
                    17 => {
                        let n = ((lx * 11 + ly * 5) % 14) as u8;
                        (122_u8.saturating_add(n), 92_u8.saturating_add(n), 64_u8)
                    }
                    // 18..=20: Gravel, Concrete Foundation, Sidewalk
                    18..=20 => {
                        let n = ((lx * 7 + ly * 19) % 12) as u8;
                        (168_u8.saturating_add(n), 170_u8.saturating_add(n), 166_u8)
                    }
                    // 24..=26: Brick Wall (Intact, Cracked, Rubble)
                    24..=26 => {
                        let mortar = (ly % 8 == 0) || ((lx + (ly / 8) * 16) % 16 == 0);
                        let (mut br, mut bg, mut bb) = if mortar {
                            (185_u8, 178_u8, 168_u8)
                        } else {
                            (168_u8, 76_u8, 56_u8)
                        };
                        // Diagonal stress fracture crack on tile 25 & 26
                        if tile_idx >= 25 && ((lx as i32 - ly as i32).abs() <= 2) {
                            br = 28;
                            bg = 24;
                            bb = 22;
                        }
                        (br, bg, bb)
                    }
                    // 27..=29: Reinforced Concrete Load-Bearing Pillar
                    27..=29 => {
                        let rebar = lx % 16 < 3 || ly % 16 < 3;
                        let (mut pr, mut pg, mut pb) = if rebar {
                            (110_u8, 118_u8, 126_u8)
                        } else {
                            (148_u8, 154_u8, 160_u8)
                        };
                        if tile_idx >= 28 && ((lx as i32 + ly as i32 - 64).abs() <= 2) {
                            pr = 30;
                            pg = 30;
                            pb = 34;
                        }
                        (pr, pg, pb)
                    }
                    // 30: Interior Checkerboard Flooring
                    30 => {
                        let checker = ((lx / 8) + (ly / 8)) % 2 == 0;
                        if checker {
                            (212_u8, 204_u8, 186_u8)
                        } else {
                            (164_u8, 156_u8, 140_u8)
                        }
                    }
                    // 31: Roof Corrugation
                    31 => {
                        let ridge = (lx % 6) < 3;
                        if ridge {
                            (118_u8, 124_u8, 132_u8)
                        } else {
                            (94_u8, 100_u8, 108_u8)
                        }
                    }
                    _ => {
                        let checker = ((lx / 8) + (ly / 8)) % 2 == 0;
                        if checker {
                            (196_u8, 188_u8, 174_u8)
                        } else {
                            (172_u8, 164_u8, 152_u8)
                        }
                    }
                };

                if is_ao_border {
                    r = (u16::from(r) * 3 / 4) as u8;
                    g = (u16::from(g) * 3 / 4) as u8;
                    b = (u16::from(b) * 3 / 4) as u8;
                }

                if let Some(slice) = pixels.get_mut(base..base + 4) {
                    slice.copy_from_slice(&[r, g, b, 255]);
                }
            }
        }
    }

    pixels
}

/// Initializes the 512x512 procedural texture sheet and 8x8 `TextureAtlasLayout`.
pub fn init_procedural_world_atlas_system(
    mut atlas: ResMut<ProceduralWorldAtlas>,
    images: Option<ResMut<Assets<Image>>>,
    layouts: Option<ResMut<Assets<TextureAtlasLayout>>>,
) {
    if atlas.texture.is_some() {
        return;
    }
    let Some(mut images) = images else {
        return;
    };

    let pixels = generate_cohesive_atlas_rgba_bytes();
    let image = Image::new(
        Extent3d {
            width: 512,
            height: 512,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );

    atlas.texture = Some(images.add(image));
    if let Some(mut layouts) = layouts {
        let layout = TextureAtlasLayout::from_grid(UVec2::splat(64), 8, 8, None, None);
        atlas.layout = Some(layouts.add(layout));
    }
}

/// Marker component for ground decal scars (`RubbleScar`, mud tracks, foundation scars).
#[derive(Component, Debug, Clone, Copy)]
pub struct GroundDecalScar {
    pub kind: TerrainTileKind,
}

/// Ring-buffer pool governing dynamic ground decals (`MAX_GROUND_DECALS = 300`).
#[derive(Resource, Debug, Clone, Default)]
pub struct GroundDecalPool {
    pub active_decals: VecDeque<Entity>,
    pub total_crushed_scars: usize,
}

impl GroundDecalPool {
    pub fn clear(&mut self) {
        self.active_decals.clear();
        self.total_crushed_scars = 0;
    }
}

/// Component for pooled weather and burst particles.
#[derive(Component, Debug, Clone, Copy)]
pub struct WorldParticle {
    pub velocity: Vec2,
    pub remaining_secs: f32,
}

/// Ring-buffer pool governing active weather and burst particles (`MAX_WORLD_PARTICLES = 160`).
#[derive(Resource, Debug, Clone, Default)]
pub struct WorldParticlePool {
    pub active_particles: VecDeque<Entity>,
    pub weather_spawn_carry: f32,
}

impl WorldParticlePool {
    pub fn clear(&mut self) {
        self.active_particles.clear();
        self.weather_spawn_carry = 0.0;
    }
}

/// Spawns a single pooled world particle within `MAX_WORLD_PARTICLES` (`160`), evicting the
/// oldest particle if at capacity.
pub fn spawn_pooled_world_particle(
    commands: &mut Commands,
    particle_pool: &mut WorldParticlePool,
    epoch: SessionEpoch,
    pos: Vec2,
    vel: Vec2,
    color: Color,
    size: Vec2,
    lifetime: f32,
) {
    while particle_pool.active_particles.len() >= MAX_WORLD_PARTICLES {
        if let Some(oldest) = particle_pool.active_particles.pop_front()
            && let Some(e_cmds) = commands.get_entity(oldest)
        {
            e_cmds.despawn_recursive();
        }
    }
    let p_ent = commands
        .spawn((
            Name::new("Pooled World Particle"),
            SiteScoped { epoch },
            WorldParticle {
                velocity: sanitize_vec2(vel, Vec2::ZERO),
                remaining_secs: lifetime.clamp(0.1, 4.0),
            },
            Sprite {
                color,
                custom_size: Some(size),
                ..Default::default()
            },
            Transform::from_xyz(pos.x, pos.y, 120.0),
            GlobalTransform::default(),
        ))
        .id();
    particle_pool.active_particles.push_back(p_ent);
}

/// Rebuilds the road spline graph and clears pools when the site starts or restarts.
pub fn sync_world_on_site_restart_system(
    mut restart_events: EventReader<SiteRestartRequested>,
    env_config: Res<EnvironmentConfig>,
    mut road_graph: ResMut<RoadSplineGraph>,
    mut spatial_grid: ResMut<SpatialHashGrid>,
    mut decal_pool: ResMut<GroundDecalPool>,
    mut particle_pool: ResMut<WorldParticlePool>,
) {
    let mut should_rebuild = road_graph.nodes.is_empty();
    for _ in restart_events.read() {
        should_rebuild = true;
    }
    if !should_rebuild {
        return;
    }

    road_graph.rebuild_for_level(env_config.level);
    spatial_grid.clear();
    decal_pool.clear();
    particle_pool.clear();
}

/// Spawns the multi-layered isometric ground tilemap (`bevy_ecs_tilemap` `TileStorage` + road/terrain
/// tiles with bitmask auto-tiling) for the active site if none are present for `SessionEpoch`.
pub fn ensure_site_ground_spawned_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    env_config: Res<EnvironmentConfig>,
    atlas: Res<ProceduralWorldAtlas>,
    existing_ground: Query<Entity, With<GroundDecalScar>>,
) {
    if existing_ground.iter().next().is_some() {
        return;
    }

    let palette = SeasonalPalette::for_season(env_config.resolved_season);

    // Base seasonal terrain backdrop
    commands.spawn((
        Name::new("Isometric Ground Base Layer"),
        GroundDecalScar {
            kind: TerrainTileKind::Grass,
        },
        SiteScoped { epoch: *epoch },
        Sprite {
            color: palette.grass_tint,
            custom_size: Some(Vec2::new(1800.0, 1080.0)),
            ..Default::default()
        },
        Transform::from_xyz(0.0, 0.0, -550.0),
        GlobalTransform::default(),
    ));

    // Spawn chunked isometric tile grid (-6..=6) with bitmask road auto-tiling and sidewalks
    let map_size = TilemapSize { x: 13, y: 13 };
    let mut tile_storage = TileStorage::empty(map_size);
    let tilemap_entity = commands
        .spawn((
            Name::new("Isometric Chunked TileStorage"),
            SiteScoped { epoch: *epoch },
            Transform::default(),
            GlobalTransform::default(),
        ))
        .id();

    for gy in -6_i32..=6_i32 {
        for gx in -6_i32..=6_i32 {
            let is_road_x = gx.abs() == 4 || gx == 0;
            let is_road_y = gy.abs() == 4 || gy == 0;
            let is_road = is_road_x || is_road_y;
            let is_sidewalk =
                !is_road && (gx.abs() == 3 || gx.abs() == 5 || gy.abs() == 3 || gy.abs() == 5);

            let (kind, raw_atlas_idx, color) = if is_road {
                let n = gy < 6 && (is_road_x || (gy + 1).abs() == 4 || (gy + 1) == 0);
                let s = gy > -6 && (is_road_x || (gy - 1).abs() == 4 || (gy - 1) == 0);
                let e = gx < 6 && (is_road_y || (gx + 1).abs() == 4 || (gx + 1) == 0);
                let w = gx > -6 && (is_road_y || (gx - 1).abs() == 4 || (gx - 1) == 0);
                let mask = compute_road_bitmask(n, e, s, w);
                (
                    TerrainTileKind::RoadTwoLane,
                    u32::from(mask),
                    Color::srgb(0.32, 0.34, 0.38),
                )
            } else if is_sidewalk {
                (
                    TerrainTileKind::Sidewalk,
                    atlas_indices::TERRAIN_SIDEWALK,
                    Color::srgb(0.68, 0.70, 0.68),
                )
            } else {
                (
                    TerrainTileKind::Grass,
                    atlas_indices::TERRAIN_GRASS,
                    palette.grass_tint,
                )
            };

            let valid_idx = validate_atlas_index(raw_atlas_idx, MAX_PROCEDURAL_ATLAS_INDEX);
            let iso_pos = grid_to_iso_world(gx, gy, 0.0);

            let mut sprite = Sprite {
                color,
                custom_size: Some(Vec2::new(ISO_TILE_WIDTH, ISO_TILE_HEIGHT)),
                ..Default::default()
            };
            if let (Some(tex), Some(layout)) = (&atlas.texture, &atlas.layout) {
                sprite.image = tex.clone();
                sprite.texture_atlas = Some(TextureAtlas {
                    layout: layout.clone(),
                    index: valid_idx as usize,
                });
            }

            let tile_pos = TilePos {
                x: (gx + 6) as u32,
                y: (gy + 6) as u32,
            };
            let tile_entity = commands
                .spawn((
                    Name::new(format!("IsoTile ({gx},{gy})")),
                    SiteScoped { epoch: *epoch },
                    kind,
                    tile_pos,
                    TilemapId(tilemap_entity),
                    TileTextureIndex(valid_idx),
                    sprite,
                    Transform::from_xyz(iso_pos.x, iso_pos.y, -480.0 + iso_pos.z * 0.01),
                    GlobalTransform::default(),
                ))
                .id();
            tile_storage.set(&tile_pos, tile_entity);
        }
    }

    if let Some(mut tm_cmds) = commands.get_entity(tilemap_entity) {
        tm_cmds.insert(tile_storage);
    }
}

/// Processes `DecalSpawnRequest` events (mud tread tracks, oil slicks, scorch marks, crushed rubble
/// scars) using the 300-slot `GroundDecalPool` ring buffer.
pub fn process_ground_decal_requests_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    mut decal_events: EventReader<DecalSpawnRequest>,
    mut decal_pool: ResMut<GroundDecalPool>,
    mut telemetry: ResMut<pd_core::DebugTelemetry>,
) {
    for req in decal_events.read() {
        let safe_pos = sanitize_vec2(req.world_pos, Vec2::ZERO);
        let safe_size = sanitize_vec2(req.size, Vec2::new(14.0, 8.0))
            .clamp(Vec2::splat(4.0), Vec2::splat(96.0));
        let safe_rot = sanitize_f32(req.rotation_rad, 0.0);

        while decal_pool.active_decals.len() >= MAX_GROUND_DECALS {
            if let Some(oldest) = decal_pool.active_decals.pop_front()
                && let Some(e_cmds) = commands.get_entity(oldest)
            {
                e_cmds.despawn_recursive();
            }
        }

        let color = match req.kind {
            DecalKind::MudTreadTrack => Color::srgba(0.24, 0.18, 0.12, 0.72),
            DecalKind::OilSlick => Color::srgba(0.08, 0.09, 0.12, 0.82),
            DecalKind::ScorchMark => Color::srgba(0.10, 0.08, 0.07, 0.88),
            DecalKind::ConcreteFoundationScar => Color::srgba(0.45, 0.46, 0.44, 0.85),
            DecalKind::CrushedRubbleScar => {
                decal_pool.total_crushed_scars = decal_pool.total_crushed_scars.saturating_add(1);
                Color::srgba(0.38, 0.35, 0.31, 0.85)
            }
        };

        let z = compute_stable_iso_z(safe_pos.x, safe_pos.y, 0.0, -420.0);
        let decal_entity = commands
            .spawn((
                Name::new("Dynamic Ground Decal"),
                SiteScoped { epoch: *epoch },
                GroundDecalScar {
                    kind: TerrainTileKind::RubbleScar,
                },
                Sprite {
                    color,
                    custom_size: Some(safe_size),
                    ..Default::default()
                },
                Transform::from_xyz(safe_pos.x, safe_pos.y, z)
                    .with_rotation(Quat::from_rotation_z(safe_rot)),
                GlobalTransform::default(),
            ))
            .id();

        decal_pool.active_decals.push_back(decal_entity);
    }

    telemetry.crushed_decal_count = decal_pool.total_crushed_scars;
}

/// Updates seasonal weather particles (`WeatherSetting::On`) and processes `ParticleBurstEvent`
/// requests (toilet/hydrant water bursts, propane explosions, diesel exhaust smoke) within the
/// `MAX_WORLD_PARTICLES` (`160`) ring buffer.
pub fn update_weather_and_burst_particles_system(
    mut commands: Commands,
    time: Res<Time>,
    epoch: Res<SessionEpoch>,
    env_config: Res<EnvironmentConfig>,
    mut burst_events: EventReader<ParticleBurstEvent>,
    mut particle_pool: ResMut<WorldParticlePool>,
    mut particles: Query<(Entity, &mut WorldParticle, &mut Transform)>,
) {
    let Ok(dt) = sanitize_dt(time.delta_secs()) else {
        return;
    };

    // 1. Advance existing particles and despawn expired ones.
    for (entity, mut p, mut tf) in &mut particles {
        p.remaining_secs -= dt;
        let vel = sanitize_vec2(p.velocity, Vec2::ZERO);
        tf.translation.x = sanitize_f32(tf.translation.x + vel.x * dt, tf.translation.x);
        tf.translation.y = sanitize_f32(tf.translation.y + vel.y * dt, tf.translation.y);

        if p.remaining_secs <= 0.0 {
            particle_pool.active_particles.retain(|&e| e != entity);
            if let Some(e_cmds) = commands.get_entity(entity) {
                e_cmds.despawn_recursive();
            }
        }
    }

    // 2. Process burst events (toilet/hydrant water bursts, propane blasts, exhaust smoke).
    for ev in burst_events.read() {
        let base_pos = sanitize_vec2(ev.world_pos, Vec2::ZERO);
        let count = ev.count.clamp(1, 16);
        for i in 0..count {
            let angle = f32::from(i) * (std::f32::consts::TAU / f32::from(count));
            let dir = Vec2::new(angle.cos(), angle.sin());
            match ev.kind {
                ParticleBurstKind::WaterSpray => {
                    spawn_pooled_world_particle(
                        &mut commands,
                        &mut particle_pool,
                        *epoch,
                        base_pos,
                        dir * 95.0 + Vec2::new(0.0, 40.0),
                        Color::srgba(0.35, 0.78, 0.98, 0.88),
                        Vec2::new(5.0, 5.0),
                        0.65,
                    );
                }
                ParticleBurstKind::PropaneExplosion => {
                    spawn_pooled_world_particle(
                        &mut commands,
                        &mut particle_pool,
                        *epoch,
                        base_pos,
                        dir * 165.0,
                        Color::srgba(1.0, 0.52, 0.12, 0.92),
                        Vec2::new(12.0, 12.0),
                        0.55,
                    );
                }
                ParticleBurstKind::DieselExhaustSmoke => {
                    spawn_pooled_world_particle(
                        &mut commands,
                        &mut particle_pool,
                        *epoch,
                        base_pos,
                        Vec2::new(-18.0, 42.0) + dir * 12.0,
                        Color::srgba(0.22, 0.22, 0.24, 0.65),
                        Vec2::new(9.0, 9.0),
                        0.75,
                    );
                }
                ParticleBurstKind::MasonryDust => {
                    spawn_pooled_world_particle(
                        &mut commands,
                        &mut particle_pool,
                        *epoch,
                        base_pos,
                        dir * 55.0,
                        Color::srgba(0.72, 0.68, 0.62, 0.70),
                        Vec2::new(8.0, 8.0),
                        0.60,
                    );
                }
            }
        }
    }

    // 3. Spawn ambient seasonal weather particles if Weather is On.
    if env_config.weather == WeatherSetting::On {
        particle_pool.weather_spawn_carry += dt * 15.0;
        let to_spawn = (particle_pool.weather_spawn_carry.floor() as u32).min(3);
        particle_pool.weather_spawn_carry -= to_spawn as f32;

        let t = time.elapsed_secs();
        for i in 0..to_spawn {
            let hash = ((t * 137.0 + (i as f32) * 89.0).sin()) * 420.0;
            let spawn_pos = Vec2::new(sanitize_f32(hash, 0.0), 320.0);
            match env_config.resolved_season {
                Season::Spring | Season::Summer | Season::Random => {
                    spawn_pooled_world_particle(
                        &mut commands,
                        &mut particle_pool,
                        *epoch,
                        spawn_pos,
                        Vec2::new(-45.0, -260.0),
                        Color::srgba(0.62, 0.80, 0.96, 0.65),
                        Vec2::new(2.0, 14.0),
                        1.4,
                    );
                }
                Season::Autumn => {
                    spawn_pooled_world_particle(
                        &mut commands,
                        &mut particle_pool,
                        *epoch,
                        spawn_pos,
                        Vec2::new(-55.0, -75.0),
                        Color::srgba(0.88, 0.44, 0.14, 0.85),
                        Vec2::new(6.0, 4.0),
                        2.5,
                    );
                }
                Season::Winter => {
                    spawn_pooled_world_particle(
                        &mut commands,
                        &mut particle_pool,
                        *epoch,
                        spawn_pos,
                        Vec2::new(-22.0, -90.0),
                        Color::srgba(0.95, 0.97, 1.0, 0.88),
                        Vec2::new(5.0, 5.0),
                        2.4,
                    );
                }
            }
        }
    }
}

/// Bevy plugin for `pd_world`.
pub struct PdWorldPlugin;

impl Plugin for PdWorldPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RoadSplineGraph>()
            .init_resource::<SpatialHashGrid>()
            .init_resource::<ProceduralWorldAtlas>()
            .init_resource::<GroundDecalPool>()
            .init_resource::<WorldParticlePool>()
            .add_systems(Startup, init_procedural_world_atlas_system)
            .add_systems(
                Update,
                (
                    sync_world_on_site_restart_system,
                    ensure_site_ground_spawned_system.run_if(in_state(AppState::Playing)),
                    process_ground_decal_requests_system.run_if(in_state(AppState::Playing)),
                    update_weather_and_burst_particles_system.run_if(in_state(AppState::Playing)),
                )
                    .chain()
                    .in_set(PdSystemSet::WorldTilemapSync),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grid_and_atlas_bounds_safety() {
        assert_eq!(validate_grid_coord(-1, 5, 32, 32), None);
        assert_eq!(validate_grid_coord(i32::MAX, i32::MIN, 32, 32), None);
        assert_eq!(validate_grid_coord(4, 7, 32, 32), Some((4, 7)));

        assert_eq!(
            validate_atlas_index(9999, MAX_PROCEDURAL_ATLAS_INDEX),
            FALLBACK_ERROR_TILE_INDEX
        );
        assert_eq!(validate_atlas_index(12, MAX_PROCEDURAL_ATLAS_INDEX), 12);

        let iso = grid_to_iso_world(10, -5, f32::NAN);
        assert!(iso.is_finite());

        let bytes = generate_cohesive_atlas_rgba_bytes();
        assert_eq!(bytes.len(), 512 * 512 * 4);
    }
}
