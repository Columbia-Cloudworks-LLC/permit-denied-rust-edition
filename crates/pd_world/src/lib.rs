//! `pd_world`: 2.5D isometric tilemap pipeline (`bevy_ecs_tilemap`), 2:1 isometric diamond ground
//! quads (`isoQuad`), oriented surface polygons (`drawOrientedGround`), 2.5D extruded isometric
//! box renderer (`drawOrientedIsoBox`) with upper-west sun lighting, procedural 64-tile texture
//! atlas (`512x512`), bitmask road auto-tiling, seasonal palette swaps, dynamic ground decal
//! ring buffer, pooled weather/burst particle system, interior cutaway masks, and spatial hash grid.
//!
//! Depends only on `pd_core`.

use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::render::primitives::Aabb;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::sprite::AlphaMode2d;
use bevy_ecs_tilemap::prelude::*;
pub use pd_core::{
    ISO_BIAS_GROUND_BACKDROP, ISO_BIAS_GROUND_DECALS, ISO_BIAS_GROUND_TILES, ISO_BIAS_PARTICLES,
    IsoBoxAssembly, IsoExtrudedBox, IsoLayerBias, MaterialKind, RoofSlopeSpec, WorldPose,
    heading_offset, iso_depth_key, iso_screen_to_world, world_to_iso_screen,
};
use pd_core::{
    AppState, CampaignLevel, DecalKind, DecalSpawnRequest, EnvironmentConfig, ParticleBurstEvent,
    ParticleBurstKind, PdSystemSet, Season, SessionEpoch, SiteRestartRequested, SiteScoped,
    SiteSeed, SplitMix64, WeatherSetting, sanitize_dt, sanitize_f32, sanitize_vec2, sanitize_vec3,
};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
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

/// Cartesian world-space width/depth of a single ground tile (`32.0` world units -> `64x32` screen diamond).
pub const WORLD_TILE_SIZE: f32 = 32.0;

/// Default spatial hash cell size (world units).
pub const SPATIAL_CELL_SIZE: f32 = 48.0;

/// Fixed isometric upper-west sun horizontal direction (`(-0.72, -0.68)` from `lighting.ts`).
pub const UPPER_WEST_SUN_XY: Vec2 = Vec2::new(-0.72, -0.68);

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
/// isometric 2D screen space with stable Z-ordering (preventing sprite Z-fighting).
#[must_use]
pub fn grid_to_iso_world(grid_x: i32, grid_y: i32, simulated_height: f32) -> Vec3 {
    let world_xy = Vec2::new(
        (grid_x as f32) * WORLD_TILE_SIZE,
        (grid_y as f32) * WORLD_TILE_SIZE,
    );
    let safe_h = sanitize_f32(simulated_height, 0.0).clamp(0.0, 500.0);
    let screen = world_to_iso_screen(world_xy, safe_h);
    let stable_z = iso_depth_key(world_xy, safe_h, 0.0);

    Vec3::new(screen.x, screen.y, stable_z)
}

/// Computes a deterministic Z depth within `[-800.0, 800.0]` to prevent sprite Z-fighting.
#[must_use]
pub fn compute_stable_iso_z(
    world_x: f32,
    ground_y: f32,
    simulated_height: f32,
    layer_bias: f32,
) -> f32 {
    iso_depth_key(
        Vec2::new(world_x, ground_y),
        simulated_height,
        layer_bias,
    )
}

/// Projects an axis-aligned Cartesian ground rectangle `(world_x, world_y, width, depth)` at
/// elevation `elev_z` into 4 isometric screen-space diamond vertices (`isoQuad` in `iso.ts`).
#[must_use]
pub fn iso_quad_vertices(
    world_x: f32,
    world_y: f32,
    width: f32,
    depth: f32,
    elev_z: f32,
) -> [Vec2; 4] {
    let x = sanitize_f32(world_x, 0.0);
    let y = sanitize_f32(world_y, 0.0);
    let w = sanitize_f32(width, 1.0).max(0.01);
    let d = sanitize_f32(depth, 1.0).max(0.01);
    let z = sanitize_f32(elev_z, 0.0);

    [
        world_to_iso_screen(Vec2::new(x, y), z),
        world_to_iso_screen(Vec2::new(x + w, y), z),
        world_to_iso_screen(Vec2::new(x + w, y + d), z),
        world_to_iso_screen(Vec2::new(x, y + d), z),
    ]
}

/// Projects an oriented Cartesian rectangle centered at `(cx, cy)` with `heading`, `length`
/// (along heading), and `width` (across heading) at elevation `elev_z` into 4 isometric
/// screen-space vertices (`drawOrientedGround` in `drawIso.ts`).
#[must_use]
pub fn oriented_ground_vertices(
    cx: f32,
    cy: f32,
    heading: f32,
    length: f32,
    width: f32,
    elev_z: f32,
) -> [Vec2; 4] {
    let scx = sanitize_f32(cx, 0.0);
    let scy = sanitize_f32(cy, 0.0);
    let sh = sanitize_f32(heading, 0.0);
    let hl = sanitize_f32(length, 1.0).max(0.01) * 0.5;
    let hw = sanitize_f32(width, 1.0).max(0.01) * 0.5;
    let z = sanitize_f32(elev_z, 0.0);
    let (fy, fx) = sh.sin_cos();

    let p0 = Vec2::new(scx + fx * hl - fy * hw, scy + fy * hl + fx * hw);
    let p1 = Vec2::new(scx + fx * hl + fy * hw, scy + fy * hl - fx * hw);
    let p2 = Vec2::new(scx - fx * hl + fy * hw, scy - fy * hl - fx * hw);
    let p3 = Vec2::new(scx - fx * hl - fy * hw, scy - fy * hl + fx * hw);

    [
        world_to_iso_screen(p0, z),
        world_to_iso_screen(p1, z),
        world_to_iso_screen(p2, z),
        world_to_iso_screen(p3, z),
    ]
}

/// Scales the sRGB channels of `base_color` by `factor` and applies `alpha`, clamping cleanly.
#[must_use]
pub fn scale_srgb_color(base_color: Color, factor: f32, alpha: f32) -> Color {
    let srgba = base_color.to_srgba();
    let f = sanitize_f32(factor, 1.0).clamp(0.0, 2.0);
    let a = sanitize_f32(alpha, srgba.alpha).clamp(0.0, 1.0);
    Color::srgba(
        (srgba.red * f).clamp(0.0, 1.0),
        (srgba.green * f).clamp(0.0, 1.0),
        (srgba.blue * f).clamp(0.0, 1.0),
        a,
    )
}

/// Computes directional industrial shading for a 3D face normal under the fixed upper-west sun
/// (`LIGHT_DIR = (-0.72, -0.68)` from `lighting.ts` and `drawIso.ts`).
///
/// - Top lid (`world_normal.z > 0.5`): receives direct upper-west sun (`~1.08x`), modulated by
///   any roof slope normal (`roofSlopeLight`).
/// - South-East face (`+X` normal): receives oblique side lighting (`~0.85x`).
/// - South-West face (`+Y` normal): lies in upper-west shadow (`~0.62x`).
#[must_use]
pub fn compute_face_lighting(base_color: Color, world_normal: Vec3, alpha: f32) -> Color {
    let safe_n = sanitize_vec3(world_normal, Vec3::Z).normalize_or_zero();
    let safe_alpha = sanitize_f32(alpha, 1.0).clamp(0.0, 1.0);

    if safe_n.z > 0.45 {
        let slope_dot = safe_n.x * UPPER_WEST_SUN_XY.x + safe_n.y * UPPER_WEST_SUN_XY.y;
        let top_mul = (1.08 + slope_dot * 0.22).clamp(0.76, 1.24);
        scale_srgb_color(base_color, top_mul, safe_alpha)
    } else {
        let n_xy = Vec2::new(safe_n.x, safe_n.y).normalize_or_zero();
        //Matches drawOrientedIsoBox in drawIso.ts: t = (nx/nl - ny/nl) * 0.5 + 0.5
        // mixing between left/dark (0.62) and right/side (0.85)
        let t = ((n_xy.x - n_xy.y) * 0.5 + 0.5).clamp(0.0, 1.0);
        let side_mul = (0.62 + (0.85 - 0.62) * t + safe_n.z * 0.08).clamp(0.45, 1.05);
        scale_srgb_color(base_color, side_mul, safe_alpha)
    }
}

/// Appends a single 2D screen-space quad (`[Vec2; 4]`) to mesh attribute buffers with guaranteed
/// counter-clockwise (CCW) triangle winding.
pub fn push_quad_ccw(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    colors: &mut Vec<[f32; 4]>,
    indices: &mut Vec<u32>,
    quad: [Vec2; 4],
    z_offset: f32,
    color: Color,
) {
    let [p0, p1, p2, p3] = quad.map(|v| sanitize_vec2(v, Vec2::ZERO));
    let safe_z = sanitize_f32(z_offset, 0.0);
    let base_idx = positions.len() as u32;
    let rgba = color.to_linear().to_f32_array();

    positions.push([p0.x, p0.y, safe_z]);
    positions.push([p1.x, p1.y, safe_z]);
    positions.push([p2.x, p2.y, safe_z]);
    positions.push([p3.x, p3.y, safe_z]);

    for _ in 0..4 {
        normals.push([0.0, 0.0, 1.0]);
        colors.push(rgba);
    }
    uvs.push([0.0, 0.0]);
    uvs.push([1.0, 0.0]);
    uvs.push([1.0, 1.0]);
    uvs.push([0.0, 1.0]);

    let e1 = p1 - p0;
    let e2 = p2 - p0;
    let cross = e1.x * e2.y - e1.y * e2.x;
    if cross >= 0.0 {
        indices.extend_from_slice(&[
            base_idx,
            base_idx.saturating_add(1),
            base_idx.saturating_add(2),
            base_idx,
            base_idx.saturating_add(2),
            base_idx.saturating_add(3),
        ]);
    } else {
        indices.extend_from_slice(&[
            base_idx,
            base_idx.saturating_add(2),
            base_idx.saturating_add(1),
            base_idx,
            base_idx.saturating_add(3),
            base_idx.saturating_add(2),
        ]);
    }
}

#[allow(clippy::too_many_arguments)]
fn push_triangle_ccw(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    colors: &mut Vec<[f32; 4]>,
    indices: &mut Vec<u32>,
    tri: [Vec2; 3],
    z_offset: f32,
    color: Color,
) {
    let [p0, p1, p2] = tri.map(|v| sanitize_vec2(v, Vec2::ZERO));
    let safe_z = sanitize_f32(z_offset, 0.0);
    let base_idx = positions.len() as u32;
    let rgba = color.to_linear().to_f32_array();

    positions.push([p0.x, p0.y, safe_z]);
    positions.push([p1.x, p1.y, safe_z]);
    positions.push([p2.x, p2.y, safe_z]);

    for _ in 0..3 {
        normals.push([0.0, 0.0, 1.0]);
        colors.push(rgba);
    }
    uvs.push([0.0, 0.0]);
    uvs.push([0.5, 1.0]);
    uvs.push([1.0, 0.0]);

    let e1 = p1 - p0;
    let e2 = p2 - p0;
    let cross = e1.x * e2.y - e1.y * e2.x;
    if cross >= 0.0 {
        indices.extend_from_slice(&[
            base_idx,
            base_idx.saturating_add(1),
            base_idx.saturating_add(2),
        ]);
    } else {
        indices.extend_from_slice(&[
            base_idx,
            base_idx.saturating_add(2),
            base_idx.saturating_add(1),
        ]);
    }
}

fn build_mesh_from_buffers(
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Builds a true 2:1 isometric diamond mesh (`isoQuad`) in local screen coordinates around
/// `origin_screen`.
#[must_use]
pub fn build_iso_ground_quad_mesh(
    world_x: f32,
    world_y: f32,
    width: f32,
    depth: f32,
    elev_z: f32,
    origin_screen: Vec2,
    color: Color,
) -> Mesh {
    let mut positions = Vec::with_capacity(4);
    let mut normals = Vec::with_capacity(4);
    let mut uvs = Vec::with_capacity(4);
    let mut colors = Vec::with_capacity(4);
    let mut indices = Vec::with_capacity(6);

    let quad = iso_quad_vertices(world_x, world_y, width, depth, elev_z)
        .map(|v| v - origin_screen);
    push_quad_ccw(
        &mut positions,
        &mut normals,
        &mut uvs,
        &mut colors,
        &mut indices,
        quad,
        0.0,
        color,
    );
    build_mesh_from_buffers(positions, normals, uvs, colors, indices)
}

/// Builds an oriented isometric ground polygon mesh (`drawOrientedGround`) in local screen
/// coordinates around `origin_screen`, optionally adding grouser tread bars for `MudTreadTrack`.
#[must_use]
pub fn build_oriented_ground_mesh(
    cx: f32,
    cy: f32,
    heading: f32,
    length: f32,
    width: f32,
    elev_z: f32,
    origin_screen: Vec2,
    color: Color,
    with_tread_grousers: bool,
) -> Mesh {
    let mut positions = Vec::with_capacity(20);
    let mut normals = Vec::with_capacity(20);
    let mut uvs = Vec::with_capacity(20);
    let mut colors = Vec::with_capacity(20);
    let mut indices = Vec::with_capacity(30);

    let base_quad =
        oriented_ground_vertices(cx, cy, heading, length, width, elev_z).map(|v| v - origin_screen);
    push_quad_ccw(
        &mut positions,
        &mut normals,
        &mut uvs,
        &mut colors,
        &mut indices,
        base_quad,
        0.0,
        color,
    );

    if with_tread_grousers {
        let groove_color = scale_srgb_color(color, 0.62, color.to_srgba().alpha * 0.88);
        let safe_len = sanitize_f32(length, 16.0).max(4.0);
        let safe_wid = sanitize_f32(width, 20.0).max(4.0);
        let bars = 3_usize;
        for i in 0..bars {
            let t = ((i as f32 + 0.5) / (bars as f32)) - 0.5;
            let bar_center = heading_offset(Vec2::new(cx, cy), heading, t * safe_len * 0.75, 0.0);
            let bar_quad = oriented_ground_vertices(
                bar_center.x,
                bar_center.y,
                heading,
                safe_len * 0.12,
                safe_wid * 0.92,
                elev_z,
            )
            .map(|v| v - origin_screen);
            push_quad_ccw(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut colors,
                &mut indices,
                bar_quad,
                0.01,
                groove_color,
            );
        }
    }

    build_mesh_from_buffers(positions, normals, uvs, colors, indices)
}

/// Builds an interlocking 2:1 isometric diamond tile mesh for a road, sidewalk, or terrain cell
/// `(gx, gy)` with bitmask road markings (curbs, dashed yellow centerlines, crosswalks) or
/// seasonal terrain speckles.
#[must_use]
pub fn build_site_tile_iso_mesh(
    gx: i32,
    gy: i32,
    kind: TerrainTileKind,
    road_mask: u8,
    base_color: Color,
) -> Mesh {
    let wx = (gx as f32) * WORLD_TILE_SIZE;
    let wy = (gy as f32) * WORLD_TILE_SIZE;
    let s = WORLD_TILE_SIZE;
    let center_world = Vec2::new(wx + s * 0.5, wy + s * 0.5);
    let origin_screen = world_to_iso_screen(center_world, 0.0);

    let mut positions = Vec::with_capacity(28);
    let mut normals = Vec::with_capacity(28);
    let mut uvs = Vec::with_capacity(28);
    let mut colors = Vec::with_capacity(28);
    let mut indices = Vec::with_capacity(42);

    // Base interlocking 2:1 isometric diamond quad (seamless with adjacent tiles)
    let base_quad = iso_quad_vertices(wx, wy, s, s, 0.0).map(|v| v - origin_screen);
    push_quad_ccw(
        &mut positions,
        &mut normals,
        &mut uvs,
        &mut colors,
        &mut indices,
        base_quad,
        0.0,
        base_color,
    );

    match kind {
        TerrainTileKind::RoadTwoLane | TerrainTileKind::RoadFourLane => {
            let curb_color = Color::srgb(0.54, 0.53, 0.50);
            let line_color = Color::srgb(0.83, 0.77, 0.42);
            let crosswalk_color = Color::srgb(0.90, 0.90, 0.88);
            let curb_w = s * 0.11;

            // North (+Y) / South (-Y) / East (+X) / West (-X) curbs on unconnected edges
            if road_mask & 1 == 0 {
                let q = iso_quad_vertices(wx, wy + s - curb_w, s, curb_w, 0.0)
                    .map(|v| v - origin_screen);
                push_quad_ccw(
                    &mut positions,
                    &mut normals,
                    &mut uvs,
                    &mut colors,
                    &mut indices,
                    q,
                    0.01,
                    curb_color,
                );
            }
            if road_mask & 4 == 0 {
                let q = iso_quad_vertices(wx, wy, s, curb_w, 0.0).map(|v| v - origin_screen);
                push_quad_ccw(
                    &mut positions,
                    &mut normals,
                    &mut uvs,
                    &mut colors,
                    &mut indices,
                    q,
                    0.01,
                    curb_color,
                );
            }
            if road_mask & 2 == 0 {
                let q = iso_quad_vertices(wx + s - curb_w, wy, curb_w, s, 0.0)
                    .map(|v| v - origin_screen);
                push_quad_ccw(
                    &mut positions,
                    &mut normals,
                    &mut uvs,
                    &mut colors,
                    &mut indices,
                    q,
                    0.01,
                    curb_color,
                );
            }
            if road_mask & 8 == 0 {
                let q = iso_quad_vertices(wx, wy, curb_w, s, 0.0).map(|v| v - origin_screen);
                push_quad_ccw(
                    &mut positions,
                    &mut normals,
                    &mut uvs,
                    &mut colors,
                    &mut indices,
                    q,
                    0.01,
                    curb_color,
                );
            }

            // Yellow dashed centerlines along connected axes
            if road_mask & 0b0101 != 0 {
                for dash in 0..2_u8 {
                    let dy = wy + s * (0.16 + f32::from(dash) * 0.44);
                    let q = iso_quad_vertices(wx + s * 0.46, dy, s * 0.08, s * 0.24, 0.0)
                        .map(|v| v - origin_screen);
                    push_quad_ccw(
                        &mut positions,
                        &mut normals,
                        &mut uvs,
                        &mut colors,
                        &mut indices,
                        q,
                        0.02,
                        line_color,
                    );
                }
            }
            if road_mask & 0b1010 != 0 {
                for dash in 0..2_u8 {
                    let dx = wx + s * (0.16 + f32::from(dash) * 0.44);
                    let q = iso_quad_vertices(dx, wy + s * 0.46, s * 0.24, s * 0.08, 0.0)
                        .map(|v| v - origin_screen);
                    push_quad_ccw(
                        &mut positions,
                        &mut normals,
                        &mut uvs,
                        &mut colors,
                        &mut indices,
                        q,
                        0.02,
                        line_color,
                    );
                }
            }
            if road_mask == 15 {
                let q = iso_quad_vertices(wx + s * 0.12, wy + s * 0.12, s * 0.76, s * 0.08, 0.0)
                    .map(|v| v - origin_screen);
                push_quad_ccw(
                    &mut positions,
                    &mut normals,
                    &mut uvs,
                    &mut colors,
                    &mut indices,
                    q,
                    0.03,
                    crosswalk_color,
                );
            }
        }
        TerrainTileKind::Sidewalk | TerrainTileKind::ConcreteFoundation => {
            let seam_color = scale_srgb_color(base_color, 0.84, 1.0);
            let q = iso_quad_vertices(wx + s * 0.47, wy + s * 0.08, s * 0.05, s * 0.84, 0.0)
                .map(|v| v - origin_screen);
            push_quad_ccw(
                &mut positions,
                &mut normals,
                &mut uvs,
                &mut colors,
                &mut indices,
                q,
                0.01,
                seam_color,
            );
        }
        TerrainTileKind::Grass | TerrainTileKind::Dirt | TerrainTileKind::Gravel | TerrainTileKind::RubbleScar => {
            // Subtle cover speckles matching drawCoverSpeckle in groundDraw.ts
            let speckle_color = scale_srgb_color(base_color, 0.82, 0.55);
            let seed = (gx.wrapping_mul(17).wrapping_add(gy.wrapping_mul(31))).unsigned_abs();
            for i in 0..2_u32 {
                let u = (((seed.wrapping_add(i * 13)) % 60) as f32 + 12.0) / 100.0;
                let v = (((seed.wrapping_add(i * 29)) % 60) as f32 + 12.0) / 100.0;
                let q = iso_quad_vertices(wx + s * u, wy + s * v, s * 0.16, s * 0.14, 0.0)
                    .map(|pt| pt - origin_screen);
                push_quad_ccw(
                    &mut positions,
                    &mut normals,
                    &mut uvs,
                    &mut colors,
                    &mut indices,
                    q,
                    0.01,
                    speckle_color,
                );
            }
        }
    }

    build_mesh_from_buffers(positions, normals, uvs, colors, indices)
}

fn eval_box_corners(
    entity_pose: &WorldPose,
    spec: &IsoExtrudedBox,
) -> (
    Vec2,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    [(Vec2, f32, f32); 4],
) {
    let box_world_center = heading_offset(
        entity_pose.pos,
        entity_pose.heading_rad,
        spec.center.x,
        spec.center.y,
    );
    let total_heading = sanitize_f32(entity_pose.heading_rad + spec.heading, 0.0);
    let total_pitch = sanitize_f32(entity_pose.pitch_rad + spec.pitch, 0.0).clamp(-1.1, 1.1);
    let total_roll = sanitize_f32(entity_pose.roll_rad + spec.roll, 0.0).clamp(-1.1, 1.1);

    let (fy, fx) = total_heading.sin_cos();
    let (rx, ry) = (-fy, fx);
    let hl = sanitize_f32(spec.length, 1.0).max(0.05) * 0.5;
    let hw = sanitize_f32(spec.width, 1.0).max(0.05) * 0.5;
    let base_z0 = sanitize_f32(entity_pose.elev_z + spec.z0, 0.0).max(0.0);
    let h = sanitize_f32(spec.height, 1.0).max(0.05);

    let eval_corner = |along: f32, across: f32| -> (Vec2, f32, f32) {
        let w = Vec2::new(
            box_world_center.x + fx * along + rx * across,
            box_world_center.y + fy * along + ry * across,
        );
        let effective_along = spec.center.x + along;
        let effective_across = spec.center.y + across;
        let tilt_dz = -effective_along * total_pitch.sin() + effective_across * total_roll.sin();
        let z_bot = (base_z0 + tilt_dz).max(0.0);
        let top_scale = match spec.roof_slope {
            None => 1.0,
            Some(RoofSlopeSpec::AcrossWidth {
                low_scale,
                high_scale,
            }) => {
                let u = ((hw - across) / (2.0 * hw).max(0.01)).clamp(0.0, 1.0);
                let lo = sanitize_f32(low_scale, 0.4);
                let hi = sanitize_f32(high_scale, 1.0);
                lo + (hi - lo) * u
            }
            Some(RoofSlopeSpec::AlongLength {
                back_scale,
                front_scale,
            }) => {
                let u = ((along + hl) / (2.0 * hl).max(0.01)).clamp(0.0, 1.0);
                let lo = sanitize_f32(back_scale, 0.4);
                let hi = sanitize_f32(front_scale, 1.0);
                lo + (hi - lo) * u
            }
            Some(RoofSlopeSpec::Gable { eave_scale, .. }) => sanitize_f32(eave_scale, 0.35),
        };
        let z_top = (z_bot + h * top_scale.max(0.02)).max(z_bot + 0.02);
        (w, z_bot, z_top)
    };

    let c0 = eval_corner(hl, -hw);
    let c1 = eval_corner(hl, hw);
    let c2 = eval_corner(-hl, hw);
    let c3 = eval_corner(-hl, -hw);
    (
        box_world_center,
        fx,
        fy,
        rx,
        ry,
        hl,
        hw,
        base_z0,
        h,
        total_pitch,
        [c0, c1, c2, c3],
    )
}

#[allow(clippy::too_many_arguments)]
fn append_extruded_box_shadow_to_buffers(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    colors: &mut Vec<[f32; 4]>,
    indices: &mut Vec<u32>,
    entity_pose: &WorldPose,
    entity_screen_origin: Vec2,
    spec: &IsoExtrudedBox,
    alpha_mul: f32,
    z_layer_offset: f32,
) {
    if !spec.cast_shadow {
        return;
    }
    let alpha = (sanitize_f32(spec.alpha, 1.0) * sanitize_f32(alpha_mul, 1.0)).clamp(0.0, 1.0);
    let (_, _, _, _, _, _, _, _, _, _, [c0, c1, c2, c3]) = eval_box_corners(entity_pose, spec);
    let shadow_quad = [
        world_to_iso_screen(c0.0, 0.0) - entity_screen_origin,
        world_to_iso_screen(c1.0, 0.0) - entity_screen_origin,
        world_to_iso_screen(c2.0, 0.0) - entity_screen_origin,
        world_to_iso_screen(c3.0, 0.0) - entity_screen_origin,
    ];
    let shadow_color = Color::srgba(0.10, 0.08, 0.05, (0.28 * alpha).clamp(0.0, 1.0));
    push_quad_ccw(
        positions,
        normals,
        uvs,
        colors,
        indices,
        shadow_quad,
        z_layer_offset - 0.10,
        shadow_color,
    );
}

#[allow(clippy::too_many_arguments)]
fn append_extruded_box_volume_to_buffers(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    colors: &mut Vec<[f32; 4]>,
    indices: &mut Vec<u32>,
    entity_pose: &WorldPose,
    entity_screen_origin: Vec2,
    spec: &IsoExtrudedBox,
    alpha_mul: f32,
    z_layer_offset: f32,
) {
    let (
        box_world_center,
        fx,
        fy,
        rx,
        ry,
        hl,
        _hw,
        base_z0,
        h,
        total_pitch,
        [c0, c1, c2, c3],
    ) = eval_box_corners(entity_pose, spec);
    let alpha = (sanitize_f32(spec.alpha, 1.0) * sanitize_f32(alpha_mul, 1.0)).clamp(0.0, 1.0);

    // 1. Visible camera-facing side faces sorted back-to-front in a stack-backed array (zero heap allocs)
    let edges = [(c0, c1), (c1, c2), (c2, c3), (c3, c0)];
    let mut visible_faces: [(f32, [Vec2; 4], Color); 4] =
        [(0.0, [Vec2::ZERO; 4], Color::NONE); 4];
    let mut face_count: usize = 0;

    for (a, b) in edges {
        let edge_vec = b.0 - a.0;
        let normal_2d = Vec2::new(edge_vec.y, -edge_vec.x);
        if normal_2d.x + normal_2d.y <= 0.001 {
            continue;
        }
        let mid_w = (a.0 + b.0) * 0.5;
        let mid_z = (a.1 + a.2 + b.1 + b.2) * 0.25;
        let depth = (mid_w.x + mid_w.y) + mid_z * 2.0;
        let normal_3d = Vec3::new(normal_2d.x, normal_2d.y, 0.0).normalize_or_zero();
        let face_color = compute_face_lighting(spec.base_color, normal_3d, alpha);

        let top_a = world_to_iso_screen(a.0, a.2) - entity_screen_origin;
        let top_b = world_to_iso_screen(b.0, b.2) - entity_screen_origin;
        let bot_b = world_to_iso_screen(b.0, b.1) - entity_screen_origin;
        let bot_a = world_to_iso_screen(a.0, a.1) - entity_screen_origin;
        if let Some(slot) = visible_faces.get_mut(face_count) {
            *slot = (depth, [top_a, top_b, bot_b, bot_a], face_color);
            face_count = face_count.saturating_add(1);
        }
    }

    if let Some(active_faces) = visible_faces.get_mut(..face_count) {
        active_faces.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    }
    for (idx, &(face_depth, quad, face_color)) in visible_faces
        .get(..face_count)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let geom_z = (face_depth - (entity_pose.pos.x + entity_pose.pos.y + entity_pose.elev_z * 2.0))
            * 0.0001
            + z_layer_offset
            + (idx as f32) * 0.002;
        push_quad_ccw(
            positions,
            normals,
            uvs,
            colors,
            indices,
            quad,
            geom_z,
            face_color,
        );
    }

    // 2. Top lid (or pitched gable roof planes with shaded gable end-cap triangles)
    let top_z_offset = z_layer_offset + 0.01;
    if let Some(RoofSlopeSpec::Gable { ridge_scale, .. }) = spec.roof_slope {
        let r_scale = sanitize_f32(ridge_scale, 1.15).max(0.1);
        let rf_world = Vec2::new(box_world_center.x + fx * hl, box_world_center.y + fy * hl);
        let rb_world = Vec2::new(box_world_center.x - fx * hl, box_world_center.y - fy * hl);
        let rf_z = (base_z0 - hl * total_pitch.sin() + h * r_scale).max(0.05);
        let rb_z = (base_z0 + hl * total_pitch.sin() + h * r_scale).max(0.05);

        let t0 = world_to_iso_screen(c0.0, c0.2) - entity_screen_origin;
        let t1 = world_to_iso_screen(c1.0, c1.2) - entity_screen_origin;
        let t2 = world_to_iso_screen(c2.0, c2.2) - entity_screen_origin;
        let t3 = world_to_iso_screen(c3.0, c3.2) - entity_screen_origin;
        let trf = world_to_iso_screen(rf_world, rf_z) - entity_screen_origin;
        let trb = world_to_iso_screen(rb_world, rb_z) - entity_screen_origin;

        // Shaded triangular gable end-caps connecting the eaves to the ridge
        let n_front = Vec3::new(fx, fy, 0.0).normalize_or_zero();
        let n_back = Vec3::new(-fx, -fy, 0.0).normalize_or_zero();
        let front_cap_color = compute_face_lighting(spec.base_color, n_front, alpha);
        let back_cap_color = compute_face_lighting(spec.base_color, n_back, alpha);
        let depth_front = rf_world.x + rf_world.y + rf_z * 2.0;
        let depth_back = rb_world.x + rb_world.y + rb_z * 2.0;
        if depth_back <= depth_front {
            push_triangle_ccw(
                positions,
                normals,
                uvs,
                colors,
                indices,
                [t2, trb, t3],
                top_z_offset - 0.002,
                back_cap_color,
            );
            push_triangle_ccw(
                positions,
                normals,
                uvs,
                colors,
                indices,
                [t0, trf, t1],
                top_z_offset - 0.001,
                front_cap_color,
            );
        } else {
            push_triangle_ccw(
                positions,
                normals,
                uvs,
                colors,
                indices,
                [t0, trf, t1],
                top_z_offset - 0.002,
                front_cap_color,
            );
            push_triangle_ccw(
                positions,
                normals,
                uvs,
                colors,
                indices,
                [t2, trb, t3],
                top_z_offset - 0.001,
                back_cap_color,
            );
        }

        let n_left = Vec3::new(-rx * 0.35, -ry * 0.35, 0.92).normalize_or_zero();
        let n_right = Vec3::new(rx * 0.35, ry * 0.35, 0.92).normalize_or_zero();
        let left_slope_color = compute_face_lighting(spec.base_color, n_left, alpha);
        let right_slope_color = compute_face_lighting(spec.base_color, n_right, alpha);

        let depth_left = (c0.0.x + c0.0.y + c3.0.x + c3.0.y) * 0.5;
        let depth_right = (c1.0.x + c1.0.y + c2.0.x + c2.0.y) * 0.5;
        if depth_left <= depth_right {
            push_quad_ccw(
                positions,
                normals,
                uvs,
                colors,
                indices,
                [t0, trf, trb, t3],
                top_z_offset,
                left_slope_color,
            );
            push_quad_ccw(
                positions,
                normals,
                uvs,
                colors,
                indices,
                [trf, t1, t2, trb],
                top_z_offset + 0.002,
                right_slope_color,
            );
        } else {
            push_quad_ccw(
                positions,
                normals,
                uvs,
                colors,
                indices,
                [trf, t1, t2, trb],
                top_z_offset,
                right_slope_color,
            );
            push_quad_ccw(
                positions,
                normals,
                uvs,
                colors,
                indices,
                [t0, trf, trb, t3],
                top_z_offset + 0.002,
                left_slope_color,
            );
        }
    } else {
        let v0 = Vec3::new(c0.0.x, c0.0.y, c0.2);
        let v1 = Vec3::new(c1.0.x, c1.0.y, c1.2);
        let v3 = Vec3::new(c3.0.x, c3.0.y, c3.2);
        let mut top_n = (v1 - v0).cross(v3 - v0).normalize_or_zero();
        if top_n.z < 0.0 {
            top_n = -top_n;
        }
        if top_n.length_squared() < 1e-4 {
            top_n = Vec3::Z;
        }
        let top_color = compute_face_lighting(spec.base_color, top_n, alpha);
        let lid_quad = [
            world_to_iso_screen(c0.0, c0.2) - entity_screen_origin,
            world_to_iso_screen(c1.0, c1.2) - entity_screen_origin,
            world_to_iso_screen(c2.0, c2.2) - entity_screen_origin,
            world_to_iso_screen(c3.0, c3.2) - entity_screen_origin,
        ];
        push_quad_ccw(
            positions,
            normals,
            uvs,
            colors,
            indices,
            lid_quad,
            top_z_offset,
            top_color,
        );
    }
}

/// Appends the shadow, visible side faces, and top/sloped lid of a single `IsoExtrudedBox`
/// (`drawOrientedIsoBox`) into the target mesh buffers in local screen coordinates around
/// `entity_screen_origin`.
#[allow(clippy::too_many_arguments)]
pub fn append_extruded_box_to_buffers(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    colors: &mut Vec<[f32; 4]>,
    indices: &mut Vec<u32>,
    entity_pose: &WorldPose,
    entity_screen_origin: Vec2,
    spec: &IsoExtrudedBox,
    alpha_mul: f32,
    z_layer_offset: f32,
) {
    append_extruded_box_shadow_to_buffers(
        positions,
        normals,
        uvs,
        colors,
        indices,
        entity_pose,
        entity_screen_origin,
        spec,
        alpha_mul,
        z_layer_offset,
    );
    append_extruded_box_volume_to_buffers(
        positions,
        normals,
        uvs,
        colors,
        indices,
        entity_pose,
        entity_screen_origin,
        spec,
        alpha_mul,
        z_layer_offset,
    );
}

/// Precomputed 3D world bounds and 2D screen bounds for a single `IsoExtrudedBox` inside an
/// assembly, enabling overlap-aware 3D ray-depth ordering.
#[derive(Debug, Clone, Copy)]
pub struct OrderedBoxEntry {
    pub spec: IsoExtrudedBox,
    pub world_center: Vec2,
    pub fx: f32,
    pub fy: f32,
    pub rx: f32,
    pub ry: f32,
    pub hl: f32,
    pub hw: f32,
    pub z_min: f32,
    pub z_max: f32,
    pub s_min: f32,
    pub s_max: f32,
    pub screen_min: Vec2,
    pub screen_max: Vec2,
    pub ray_depth: f32,
}

impl OrderedBoxEntry {
    #[must_use]
    pub fn from_box(entity_pose: &WorldPose, spec: IsoExtrudedBox) -> Self {
        let (world_center, fx, fy, rx, ry, hl, hw, base_z0, h, _, corners) =
            eval_box_corners(entity_pose, &spec);

        let mut z_min = f32::MAX;
        let mut z_max = f32::MIN;
        let mut s_min = f32::MAX;
        let mut s_max = f32::MIN;
        let mut screen_min = Vec2::splat(f32::MAX);
        let mut screen_max = Vec2::splat(f32::MIN);

        for (w, zb, zt) in corners {
            let s = w.x + w.y;
            s_min = s_min.min(s);
            s_max = s_max.max(s);
            z_min = z_min.min(zb);
            z_max = z_max.max(zt);

            let sb = world_to_iso_screen(w, zb);
            let st = world_to_iso_screen(w, zt);
            screen_min = screen_min.min(sb).min(st);
            screen_max = screen_max.max(sb).max(st);
        }

        if let Some(RoofSlopeSpec::Gable { ridge_scale, .. }) = spec.roof_slope {
            let r_scale = sanitize_f32(ridge_scale, 1.15).max(0.1);
            let ridge_top = (base_z0 + h * r_scale).max(z_max);
            z_max = z_max.max(ridge_top);
            let rf_s = world_to_iso_screen(
                Vec2::new(world_center.x + fx * hl, world_center.y + fy * hl),
                ridge_top,
            );
            let rb_s = world_to_iso_screen(
                Vec2::new(world_center.x - fx * hl, world_center.y - fy * hl),
                ridge_top,
            );
            screen_min = screen_min.min(rf_s).min(rb_s);
            screen_max = screen_max.max(rf_s).max(rb_s);
        }

        let z_mid = (z_min + z_max) * 0.5;
        let ray_depth = (world_center.x + world_center.y) + 2.0 * z_mid;

        Self {
            spec,
            world_center,
            fx,
            fy,
            rx,
            ry,
            hl,
            hw,
            z_min,
            z_max,
            s_min,
            s_max,
            screen_min,
            screen_max,
            ray_depth,
        }
    }

    /// Computes the `[z_enter, z_exit]` interval along the isometric viewing ray passing through
    /// `screen_pt` (`world_to_iso_screen` coordinates). Larger `z` along a shared screen ray is
    /// strictly closer to the camera.
    #[must_use]
    pub fn ray_interval_at_screen_point(&self, screen_pt: Vec2) -> Option<(f32, f32)> {
        let mut t0 = self.z_min;
        let mut t1 = self.z_max;

        let p0 = Vec2::new(
            0.5 * screen_pt.x - screen_pt.y,
            -0.5 * screen_pt.x - screen_pt.y,
        );
        let d0 = p0 - self.world_center;

        let clip_slab = |a0: f32, ad: f32, half_ext: f32, t_min: &mut f32, t_max: &mut f32| -> bool {
            if ad.abs() < 1e-5 {
                a0.abs() <= half_ext + 1e-3
            } else {
                let inv = 1.0 / ad;
                let u0 = (-half_ext - a0) * inv;
                let u1 = (half_ext - a0) * inv;
                let (lo, hi) = if u0 <= u1 { (u0, u1) } else { (u1, u0) };
                *t_min = t_min.max(lo);
                *t_max = t_max.min(hi);
                *t_min <= *t_max + 0.05
            }
        };

        let a0 = d0.x * self.fx + d0.y * self.fy;
        let ad = self.fx + self.fy;
        if !clip_slab(a0, ad, self.hl, &mut t0, &mut t1) {
            return None;
        }

        let b0 = d0.x * self.rx + d0.y * self.ry;
        let bd = self.rx + self.ry;
        if !clip_slab(b0, bd, self.hw, &mut t0, &mut t1) {
            return None;
        }

        Some((t0, t1.max(t0)))
    }
}

fn compare_overlapping_boxes(a: &OrderedBoxEntry, b: &OrderedBoxEntry) -> Option<Ordering> {
    let ov_min = a.screen_min.max(b.screen_min);
    let ov_max = a.screen_max.min(b.screen_max);
    if ov_min.x >= ov_max.x - 0.05 || ov_min.y >= ov_max.y - 0.05 {
        return None;
    }

    // 1. Non-overlapping vertical (Z) separation
    if b.z_min >= a.z_max - 0.15 && b.z_max > a.z_max {
        return Some(Ordering::Less);
    }
    if a.z_min >= b.z_max - 0.15 && a.z_max > b.z_max {
        return Some(Ordering::Greater);
    }

    // 2. Non-overlapping horizontal diagonal (s = x + y) separation
    if b.s_min >= a.s_max - 0.15 && b.s_max > a.s_max {
        return Some(Ordering::Less);
    }
    if a.s_min >= b.s_max - 0.15 && a.s_max > b.s_max {
        return Some(Ordering::Greater);
    }

    // 3. Exact ray-slab depth comparison at sample points inside the 2D screen overlap region
    let mid = (ov_min + ov_max) * 0.5;
    let q1 = ov_min * 0.65 + ov_max * 0.35;
    let q2 = ov_min * 0.35 + ov_max * 0.65;
    let q3 = Vec2::new(q1.x, q2.y);
    let q4 = Vec2::new(q2.x, q1.y);
    for pt in [mid, q1, q2, q3, q4] {
        if let (Some((a_t0, a_t1)), Some((b_t0, b_t1))) = (
            a.ray_interval_at_screen_point(pt),
            b.ray_interval_at_screen_point(pt),
        ) {
            let a_key = a_t1 + a_t0 * 0.25;
            let b_key = b_t1 + b_t0 * 0.25;
            if (a_key - b_key).abs() > 0.05 {
                return Some(a_key.total_cmp(&b_key));
            }
        }
    }

    Some(a.ray_depth.total_cmp(&b.ray_depth))
}

/// Reusable scratch buffer for sorting multi-box assemblies without per-frame heap allocations.
#[derive(Debug, Default, Clone)]
pub struct IsoMeshBuildScratch {
    pub ordered_boxes: Vec<OrderedBoxEntry>,
}

/// Populates and sorts `out` in back-to-front painter's order for `pose`, `root_box`, and `assembly`.
pub fn order_assembly_boxes_for_pose(
    pose: &WorldPose,
    root_box: &IsoExtrudedBox,
    assembly: Option<&IsoBoxAssembly>,
    out: &mut Vec<OrderedBoxEntry>,
) {
    let safe_pose = pose.sanitized();
    out.clear();
    let extra_count = assembly.map_or(0, |a| a.parts.len());
    let total_boxes = 1_usize.saturating_add(extra_count);
    if out.capacity() < total_boxes {
        out.reserve(total_boxes.saturating_sub(out.capacity()));
    }

    out.push(OrderedBoxEntry::from_box(&safe_pose, *root_box));
    if let Some(asm) = assembly {
        for part in &asm.parts {
            out.push(OrderedBoxEntry::from_box(&safe_pose, *part));
        }
    }

    out.sort_by(|a, b| a.ray_depth.total_cmp(&b.ray_depth));

    let len = out.len();
    for _ in 0..len {
        let mut swapped = false;
        for i in 0..len {
            for j in (i + 1)..len {
                let should_swap = if let (Some(bi), Some(bj)) = (out.get(i), out.get(j)) {
                    compare_overlapping_boxes(bi, bj) == Some(Ordering::Greater)
                } else {
                    false
                };
                if should_swap {
                    out.swap(i, j);
                    swapped = true;
                }
            }
        }
        if !swapped {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn populate_extruded_box_buffers_with_scratch(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    colors: &mut Vec<[f32; 4]>,
    indices: &mut Vec<u32>,
    scratch: &mut IsoMeshBuildScratch,
    pose: &WorldPose,
    root_box: &IsoExtrudedBox,
    assembly: Option<&IsoBoxAssembly>,
) {
    let safe_pose = pose.sanitized();
    let entity_screen_origin = world_to_iso_screen(safe_pose.pos, safe_pose.elev_z);
    let alpha_mul = assembly.map_or(1.0, |a| sanitize_f32(a.alpha_multiplier, 1.0));

    order_assembly_boxes_for_pose(&safe_pose, root_box, assembly, &mut scratch.ordered_boxes);

    positions.clear();
    normals.clear();
    uvs.clear();
    colors.clear();
    indices.clear();

    // Pass 1: Emit all ground drop-shadows first so shadows never draw over 3D walls/roofs
    for (idx, entry) in scratch.ordered_boxes.iter().enumerate() {
        append_extruded_box_shadow_to_buffers(
            positions,
            normals,
            uvs,
            colors,
            indices,
            &safe_pose,
            entity_screen_origin,
            &entry.spec,
            alpha_mul,
            (idx as f32) * 0.001,
        );
    }

    // Pass 2: Emit 3D box volumes in overlap-aware back-to-front painter's order
    for (idx, entry) in scratch.ordered_boxes.iter().enumerate() {
        append_extruded_box_volume_to_buffers(
            positions,
            normals,
            uvs,
            colors,
            indices,
            &safe_pose,
            entity_screen_origin,
            &entry.spec,
            alpha_mul,
            (idx as f32) * 0.02,
        );
    }
}

/// Updates an existing `Mesh` asset in place by extracting and reusing its attribute/index `Vec`
/// allocations alongside `scratch`, avoiding per-frame heap churn in hot loops.
pub fn update_iso_extruded_box_mesh_in_place(
    mesh: &mut Mesh,
    scratch: &mut IsoMeshBuildScratch,
    pose: &WorldPose,
    root_box: &IsoExtrudedBox,
    assembly: Option<&IsoBoxAssembly>,
) {
    let total_boxes = 1_usize.saturating_add(assembly.map_or(0, |a| a.parts.len()));
    let mut positions = match mesh.remove_attribute(Mesh::ATTRIBUTE_POSITION) {
        Some(VertexAttributeValues::Float32x3(v)) => v,
        _ => Vec::with_capacity(total_boxes * 16),
    };
    let mut normals = match mesh.remove_attribute(Mesh::ATTRIBUTE_NORMAL) {
        Some(VertexAttributeValues::Float32x3(v)) => v,
        _ => Vec::with_capacity(total_boxes * 16),
    };
    let mut uvs = match mesh.remove_attribute(Mesh::ATTRIBUTE_UV_0) {
        Some(VertexAttributeValues::Float32x2(v)) => v,
        _ => Vec::with_capacity(total_boxes * 16),
    };
    let mut colors = match mesh.remove_attribute(Mesh::ATTRIBUTE_COLOR) {
        Some(VertexAttributeValues::Float32x4(v)) => v,
        _ => Vec::with_capacity(total_boxes * 16),
    };
    let mut indices = match mesh.remove_indices() {
        Some(Indices::U32(v)) => v,
        _ => Vec::with_capacity(total_boxes * 24),
    };

    populate_extruded_box_buffers_with_scratch(
        &mut positions,
        &mut normals,
        &mut uvs,
        &mut colors,
        &mut indices,
        scratch,
        pose,
        root_box,
        assembly,
    );

    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
}

/// Builds a complete 2.5D extruded isometric `Mesh` (`drawOrientedIsoBox`) for an entity's
/// primary `IsoExtrudedBox` and optional multi-part `IsoBoxAssembly` using reusable `scratch`.
#[must_use]
pub fn build_iso_extruded_box_mesh_with_scratch(
    scratch: &mut IsoMeshBuildScratch,
    pose: &WorldPose,
    root_box: &IsoExtrudedBox,
    assembly: Option<&IsoBoxAssembly>,
) -> Mesh {
    let total_boxes = 1_usize.saturating_add(assembly.map_or(0, |a| a.parts.len()));
    let mut positions = Vec::with_capacity(total_boxes * 16);
    let mut normals = Vec::with_capacity(total_boxes * 16);
    let mut uvs = Vec::with_capacity(total_boxes * 16);
    let mut colors = Vec::with_capacity(total_boxes * 16);
    let mut indices = Vec::with_capacity(total_boxes * 24);

    populate_extruded_box_buffers_with_scratch(
        &mut positions,
        &mut normals,
        &mut uvs,
        &mut colors,
        &mut indices,
        scratch,
        pose,
        root_box,
        assembly,
    );

    build_mesh_from_buffers(positions, normals, uvs, colors, indices)
}

/// Builds a complete 2.5D extruded isometric `Mesh` (`drawOrientedIsoBox`) for an entity's
/// primary `IsoExtrudedBox` and optional multi-part `IsoBoxAssembly`, sorted in painter's order.
#[must_use]
pub fn build_iso_extruded_box_mesh(
    pose: &WorldPose,
    root_box: &IsoExtrudedBox,
    assembly: Option<&IsoBoxAssembly>,
) -> Mesh {
    let mut scratch = IsoMeshBuildScratch::default();
    build_iso_extruded_box_mesh_with_scratch(&mut scratch, pose, root_box, assembly)
}

/// Shared vertex-color `ColorMaterial` handle used by all 2.5D isometric meshes.
#[derive(Resource, Debug, Clone, Default)]
pub struct SharedIsoColorMaterial {
    pub handle: Option<Handle<ColorMaterial>>,
}

/// Cached quantized pose & style signature on an entity so static or purely-translating isometric
/// meshes are not rebuilt unnecessarily every frame.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct IsoMeshCacheKey {
    pub heading_step: i32,
    pub pitch_step: i32,
    pub roll_step: i32,
    pub elev_step: i32,
    pub z0_step: i32,
    pub alpha_step: i32,
    pub part_count: usize,
    pub color_bits: u32,
    pub assembly_hash: u64,
}

impl IsoMeshCacheKey {
    #[must_use]
    pub fn compute(
        pose: &WorldPose,
        root_box: &IsoExtrudedBox,
        assembly: Option<&IsoBoxAssembly>,
    ) -> Self {
        let mix = |acc: u64, val: u64| -> u64 {
            let mut z = acc.wrapping_add(val).wrapping_add(0x9E37_79B9_7F4A_7C15);
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let quant = |v: f32, scale: f32| -> u64 {
            (sanitize_f32(v, 0.0) * scale).round() as i64 as u64
        };
        let hash_box = |acc: u64, b: &IsoExtrudedBox| -> u64 {
            let c = b.base_color.to_srgba();
            let cr = (c.red.clamp(0.0, 1.0) * 255.0).round() as u64;
            let cg = (c.green.clamp(0.0, 1.0) * 255.0).round() as u64;
            let cb = (c.blue.clamp(0.0, 1.0) * 255.0).round() as u64;
            let ca = (c.alpha.clamp(0.0, 1.0) * 255.0).round() as u64;
            let color_u64 = (cr << 24) | (cg << 16) | (cb << 8) | ca;

            let mut v = mix(acc, quant(b.center.x, 100.0));
            v = mix(v, quant(b.center.y, 100.0));
            v = mix(v, quant(b.heading, 1000.0));
            v = mix(v, quant(b.pitch, 1000.0));
            v = mix(v, quant(b.roll, 1000.0));
            v = mix(v, quant(b.length, 100.0));
            v = mix(v, quant(b.width, 100.0));
            v = mix(v, quant(b.z0, 100.0));
            v = mix(v, quant(b.height, 100.0));
            v = mix(v, quant(b.alpha, 1000.0));
            v = mix(v, u64::from(b.cast_shadow));
            v = mix(v, color_u64);
            match b.roof_slope {
                None => mix(v, 0),
                Some(RoofSlopeSpec::AcrossWidth {
                    low_scale,
                    high_scale,
                }) => {
                    let s1 = mix(v, 1);
                    let s2 = mix(s1, quant(low_scale, 1000.0));
                    mix(s2, quant(high_scale, 1000.0))
                }
                Some(RoofSlopeSpec::AlongLength {
                    back_scale,
                    front_scale,
                }) => {
                    let s1 = mix(v, 2);
                    let s2 = mix(s1, quant(back_scale, 1000.0));
                    mix(s2, quant(front_scale, 1000.0))
                }
                Some(RoofSlopeSpec::Gable {
                    eave_scale,
                    ridge_scale,
                }) => {
                    let s1 = mix(v, 3);
                    let s2 = mix(s1, quant(eave_scale, 1000.0));
                    mix(s2, quant(ridge_scale, 1000.0))
                }
            }
        };

        let srgba = root_box.base_color.to_srgba();
        let r = (srgba.red.clamp(0.0, 1.0) * 255.0).round() as u32;
        let g = (srgba.green.clamp(0.0, 1.0) * 255.0).round() as u32;
        let b = (srgba.blue.clamp(0.0, 1.0) * 255.0).round() as u32;
        let color_bits = (r << 16) | (g << 8) | b;
        let asm_alpha = assembly.map_or(1.0, |a| sanitize_f32(a.alpha_multiplier, 1.0));
        let mut assembly_hash = mix(0x517C_C1B7_2722_0A95, quant(asm_alpha, 1000.0));
        assembly_hash = hash_box(assembly_hash, root_box);
        if let Some(asm) = assembly {
            for part in &asm.parts {
                assembly_hash = hash_box(assembly_hash, part);
            }
        }
        Self {
            heading_step: (sanitize_f32(pose.heading_rad + root_box.heading, 0.0) * 100.0).round()
                as i32,
            pitch_step: (sanitize_f32(pose.pitch_rad + root_box.pitch, 0.0) * 100.0).round() as i32,
            roll_step: (sanitize_f32(pose.roll_rad + root_box.roll, 0.0) * 100.0).round() as i32,
            elev_step: (sanitize_f32(pose.elev_z, 0.0) * 4.0).round() as i32,
            z0_step: (sanitize_f32(root_box.z0, 0.0) * 10.0).round() as i32,
            alpha_step: (sanitize_f32(root_box.alpha * asm_alpha, 1.0) * 100.0).round() as i32,
            part_count: assembly.map_or(0, |a| a.parts.len()),
            color_bits,
            assembly_hash,
        }
    }
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
    /// Weather spawn serial since the last restart; with `SiteSeed` it seeds spawn positions.
    pub weather_spawn_serial: u64,
}

impl WorldParticlePool {
    pub fn clear(&mut self) {
        self.active_particles.clear();
        self.weather_spawn_carry = 0.0;
        self.weather_spawn_serial = 0;
    }
}

/// Persistent collapsed building foundation scar (`CollapsedSite` in `site.ts` / Issue #16).
#[derive(Component, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CollapsedSite {
    pub footprint_half_extents: Vec2,
    pub dominant_material: MaterialKind,
    pub rubble_mass: f32,
}

/// Single grid cell in the pre-allocated 2D `PileField` height/resistance field (`pile.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PileCell {
    pub pile_mass: f32,
    pub height: f32,
    pub compact: f32,
    pub material: MaterialKind,
}

impl Default for PileCell {
    fn default() -> Self {
        Self {
            pile_mass: 0.0,
            height: 0.0,
            compact: 0.0,
            material: MaterialKind::Concrete,
        }
    }
}

/// 3x3 normalized distribution kernel for `PileField::add_mass` (`sum == 1.0`).
pub const PILE_KERNEL_3X3: [(i32, i32, f32); 9] = [
    (-1, -1, 0.06),
    (0, -1, 0.12),
    (1, -1, 0.06),
    (-1, 0, 0.12),
    (0, 0, 0.28),
    (1, 0, 0.12),
    (-1, 1, 0.06),
    (0, 1, 0.12),
    (1, 1, 0.06),
];

/// Pre-allocated 2D height, compaction, and resistance field for settled/crushed rubble mass
/// (`PileField` in `pile.ts` / Issue #16). Zero heap allocations occur after initialization.
#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
pub struct PileField {
    pub cols: usize,
    pub rows: usize,
    pub cell_size: f32,
    pub origin_xy: Vec2,
    pub cells: Vec<PileCell>,
}

impl Default for PileField {
    fn default() -> Self {
        let cols: usize = 128;
        let rows: usize = 128;
        let cell_size: f32 = 16.0;
        let origin_xy = Vec2::new(-1024.0, -1024.0);
        Self {
            cols,
            rows,
            cell_size,
            origin_xy,
            cells: vec![PileCell::default(); cols.saturating_mul(rows)],
        }
    }
}

impl PileField {
    /// Resets all cells in-place without reallocating.
    pub fn clear(&mut self) {
        for cell in &mut self.cells {
            *cell = PileCell::default();
        }
    }

    /// Converts a world-space 2D coordinate into clamped interior grid indices `(gx, gy)`
    /// (`[1, cols - 2]`, `[1, rows - 2]`) so 3x3 kernel updates always stay in-bounds and conserve mass.
    #[must_use]
    pub fn world_to_interior_cell(&self, world_pos: Vec2) -> (usize, usize) {
        let safe_pos = sanitize_vec2(world_pos, Vec2::ZERO);
        let safe_cs = sanitize_f32(self.cell_size, 16.0).max(1.0);
        let rel = (safe_pos - self.origin_xy) / safe_cs;
        let max_gx = self.cols.saturating_sub(2).max(1);
        let max_gy = self.rows.saturating_sub(2).max(1);
        let gx = (rel.x.floor() as isize).clamp(1, max_gx as isize) as usize;
        let gy = (rel.y.floor() as isize).clamp(1, max_gy as isize) as usize;
        (gx, gy)
    }

    /// Converts a world-space 2D coordinate into grid indices `(gx, gy)` clamped to `[0, cols - 1]`.
    #[must_use]
    pub fn world_to_cell(&self, world_pos: Vec2) -> (usize, usize) {
        let safe_pos = sanitize_vec2(world_pos, Vec2::ZERO);
        let safe_cs = sanitize_f32(self.cell_size, 16.0).max(1.0);
        let rel = (safe_pos - self.origin_xy) / safe_cs;
        let max_gx = self.cols.saturating_sub(1);
        let max_gy = self.rows.saturating_sub(1);
        let gx = (rel.x.floor() as isize).clamp(0, max_gx as isize) as usize;
        let gy = (rel.y.floor() as isize).clamp(0, max_gy as isize) as usize;
        (gx, gy)
    }

    #[must_use]
    pub fn cell_index(&self, gx: usize, gy: usize) -> Option<usize> {
        if gx < self.cols && gy < self.rows {
            gy.checked_mul(self.cols)?.checked_add(gx)
        } else {
            None
        }
    }

    /// Deposits `mass` of `material` at `world_pos` across a 3x3 weighted kernel (`addPileMass` in `pile.ts`),
    /// strictly conserving total mass (`self.total_mass()` increases by `mass`).
    pub fn add_mass(&mut self, world_pos: Vec2, mass: f32, material: MaterialKind) {
        let safe_mass = sanitize_f32(mass, 0.0);
        if safe_mass <= 0.0 {
            return;
        }
        let (cx, cy) = self.world_to_interior_cell(world_pos);
        for (dx, dy, weight) in PILE_KERNEL_3X3 {
            let nx = (cx as i32).saturating_add(dx).max(0) as usize;
            let ny = (cy as i32).saturating_add(dy).max(0) as usize;
            if let Some(idx) = self.cell_index(nx, ny)
                && let Some(cell) = self.cells.get_mut(idx)
            {
                let add = safe_mass * weight;
                cell.pile_mass = sanitize_f32(cell.pile_mass + add, cell.pile_mass);
                let gain = add * 1.35 * (1.0 - 0.38 * cell.compact).clamp(0.25, 1.0);
                cell.height = sanitize_f32(cell.height + gain, cell.height).clamp(0.0, 28.0);
                cell.compact = sanitize_f32(cell.compact - add * 0.08, cell.compact).clamp(0.0, 1.0);
                if add > 0.04 || cell.pile_mass <= add * 1.5 {
                    cell.material = material;
                }
            }
        }
    }

    /// Returns the pile height (`>= 0.0`) at `world_pos` (`pileHeightAt` in `pile.ts`).
    #[must_use]
    pub fn height_at(&self, world_pos: Vec2) -> f32 {
        let (gx, gy) = self.world_to_cell(world_pos);
        self.cell_index(gx, gy)
            .and_then(|idx| self.cells.get(idx))
            .map_or(0.0, |c| sanitize_f32(c.height, 0.0).max(0.0))
    }

    /// Returns the physical pile drag/resistance (`>= 0.0`) at `world_pos` (`pileResistanceAt` in `pile.ts`).
    #[must_use]
    pub fn pile_resistance_at(&self, world_pos: Vec2) -> f32 {
        let (gx, gy) = self.world_to_cell(world_pos);
        self.cell_index(gx, gy)
            .and_then(|idx| self.cells.get(idx))
            .map_or(0.0, |c| {
                if c.pile_mass <= 1e-5 && c.height <= 1e-5 {
                    0.0
                } else {
                    let raw = c.height * 0.42 + c.compact * 1.45 + c.pile_mass * 0.55;
                    sanitize_f32(raw, 0.0).clamp(0.0, 12.0)
                }
            })
    }

    /// Compacts the pile cell at `world_pos` under dozer tracks or high blade without losing mass.
    pub fn compact_point(&mut self, world_pos: Vec2, amount: f32) {
        let safe_amt = sanitize_f32(amount, 0.0).clamp(0.0, 1.0);
        if safe_amt <= 0.0 {
            return;
        }
        let (gx, gy) = self.world_to_cell(world_pos);
        if let Some(idx) = self.cell_index(gx, gy)
            && let Some(cell) = self.cells.get_mut(idx)
            && (cell.pile_mass > 0.0 || cell.height > 0.0)
        {
            cell.compact = sanitize_f32(cell.compact + safe_amt, cell.compact).clamp(0.0, 1.0);
            cell.height = sanitize_f32(
                cell.height * (1.0 - safe_amt * 0.16).clamp(0.72, 1.0),
                cell.height,
            )
            .max(0.0);
        }
    }

    /// Extracts up to `max_extract` loose pile mass within `radius` of `center` (`extractPileDisk` in `pile.ts`),
    /// returning `(extracted_mass, dominant_material)`.
    pub fn extract_disk(
        &mut self,
        center: Vec2,
        radius: f32,
        max_extract: f32,
    ) -> (f32, MaterialKind) {
        let safe_center = sanitize_vec2(center, Vec2::ZERO);
        let safe_radius = sanitize_f32(radius, 16.0).clamp(1.0, 96.0);
        let budget = sanitize_f32(max_extract, 0.0).max(0.0);
        if budget <= 0.0 {
            return (0.0, MaterialKind::Concrete);
        }

        let (cx, cy) = self.world_to_interior_cell(safe_center);
        let cell_span = ((safe_radius / self.cell_size.max(1.0)).ceil() as i32).clamp(1, 3);
        let mut extracted_total = 0.0_f32;
        let mut dom_mat = MaterialKind::Concrete;

        for dy in -cell_span..=cell_span {
            for dx in -cell_span..=cell_span {
                if extracted_total >= budget {
                    break;
                }
                let nx = (cx as i32).saturating_add(dx).max(0) as usize;
                let ny = (cy as i32).saturating_add(dy).max(0) as usize;
                if let Some(idx) = self.cell_index(nx, ny)
                    && let Some(cell) = self.cells.get_mut(idx)
                    && cell.pile_mass > 0.01
                {
                    let take = (cell.pile_mass * 0.35).min(budget - extracted_total);
                    if take > 0.0 {
                        let prev_mass = cell.pile_mass.max(take);
                        cell.pile_mass = (cell.pile_mass - take).max(0.0);
                        let ratio = (cell.pile_mass / prev_mass).clamp(0.0, 1.0);
                        cell.height *= ratio;
                        extracted_total += take;
                        dom_mat = cell.material;
                    }
                }
            }
        }

        (sanitize_f32(extracted_total, 0.0), dom_mat)
    }

    /// Scoops pile mass under a lowered blade at `blade_center` and pushes it forward along
    /// `forward_dir` while strictly conserving total `PileField` mass.
    pub fn push_pile_with_blade(
        &mut self,
        blade_center: Vec2,
        forward_dir: Vec2,
        radius: f32,
        max_transfer: f32,
    ) {
        let dir = sanitize_vec2(forward_dir, Vec2::X).normalize_or_zero();
        if dir.length_squared() <= 0.1 {
            return;
        }
        let (extracted, mat) = self.extract_disk(blade_center, radius, max_transfer);
        if extracted > 0.0 {
            let deposit_pos = blade_center + dir * (radius.max(12.0) * 1.15);
            self.add_mass(deposit_pos, extracted, mat);
        }
        self.compact_point(blade_center, 0.08);
    }

    /// Returns the total mass across all cells in the `PileField`.
    #[must_use]
    pub fn total_mass(&self) -> f32 {
        let sum: f32 = self
            .cells
            .iter()
            .map(|c| sanitize_f32(c.pile_mass, 0.0).max(0.0))
            .sum();
        sanitize_f32(sum, 0.0)
    }

    /// Returns the number of cells containing measurable pile mass (`> 0.05`).
    #[must_use]
    pub fn active_mound_cells(&self) -> usize {
        self.cells.iter().filter(|c| c.pile_mass > 0.05).count()
    }
}

/// Helper that returns or initializes the shared vertex-color `ColorMaterial` handle.
pub fn get_or_init_shared_iso_material(
    shared_mat: &mut SharedIsoColorMaterial,
    materials: &mut Assets<ColorMaterial>,
) -> Handle<ColorMaterial> {
    if let Some(handle) = &shared_mat.handle {
        return handle.clone();
    }
    let handle = materials.add(ColorMaterial {
        color: Color::WHITE,
        alpha_mode: AlphaMode2d::Blend,
        texture: None,
    });
    shared_mat.handle = Some(handle.clone());
    handle
}

/// Spawns a single pooled world particle within `MAX_WORLD_PARTICLES` (`160`), evicting the
/// oldest particle if at capacity.
#[allow(clippy::too_many_arguments)]
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
    let safe_pos = sanitize_vec2(pos, Vec2::ZERO);
    let safe_size = sanitize_vec2(size, Vec2::splat(4.0)).clamp(Vec2::splat(1.5), Vec2::splat(24.0));
    let screen = world_to_iso_screen(safe_pos, 14.0);
    let z = iso_depth_key(safe_pos, 14.0, ISO_BIAS_PARTICLES);

    let p_ent = commands
        .spawn((
            Name::new("Pooled World Particle"),
            SiteScoped { epoch },
            WorldParticle {
                velocity: sanitize_vec2(vel, Vec2::ZERO),
                remaining_secs: lifetime.clamp(0.1, 4.0),
            },
            WorldPose::new(safe_pos, 14.0, 0.35),
            IsoLayerBias(ISO_BIAS_PARTICLES),
            IsoExtrudedBox::new(
                safe_size.x,
                safe_size.y,
                (safe_size.y * 0.45).max(1.5),
                color,
            )
            .with_heading(0.35)
            .with_alpha(color.to_srgba().alpha)
            .with_shadow(false),
            Transform::from_xyz(screen.x, screen.y, z),
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
    mut pile_field: ResMut<PileField>,
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
    pile_field.clear();
}

/// Spawns the multi-layered isometric ground tilemap (`bevy_ecs_tilemap` `TileStorage` + interlocking
/// 2:1 isometric diamond road/terrain tiles with bitmask auto-tiling) for the active site if none
/// are present for `SessionEpoch`.
pub fn ensure_site_ground_spawned_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    env_config: Res<EnvironmentConfig>,
    mut shared_mat: ResMut<SharedIsoColorMaterial>,
    mut meshes: Option<ResMut<Assets<Mesh>>>,
    mut materials: Option<ResMut<Assets<ColorMaterial>>>,
    existing_ground: Query<Entity, With<GroundDecalScar>>,
) {
    if existing_ground.iter().next().is_some() {
        return;
    }

    let palette = SeasonalPalette::for_season(env_config.resolved_season);
    let mat_handle = materials
        .as_deref_mut()
        .map(|mats| get_or_init_shared_iso_material(&mut shared_mat, mats));

    // Base seasonal terrain diamond backdrop (-1100..+1100 world units)
    let mut base_cmds = commands.spawn((
        Name::new("Isometric Ground Base Layer"),
        GroundDecalScar {
            kind: TerrainTileKind::Grass,
        },
        SiteScoped { epoch: *epoch },
        WorldPose::from_xy(0.0, 0.0),
        IsoLayerBias(ISO_BIAS_GROUND_BACKDROP),
        Transform::from_xyz(0.0, 0.0, ISO_BIAS_GROUND_BACKDROP),
        GlobalTransform::default(),
    ));
    if let Some(meshes_mut) = meshes.as_deref_mut() {
        let backdrop_mesh = build_iso_ground_quad_mesh(
            -1100.0,
            -1100.0,
            2200.0,
            2200.0,
            0.0,
            Vec2::ZERO,
            scale_srgb_color(palette.grass_tint, 0.88, 1.0),
        );
        base_cmds.insert(Mesh2d(meshes_mut.add(backdrop_mesh)));
        if let Some(mat) = &mat_handle {
            base_cmds.insert(MeshMaterial2d(mat.clone()));
        }
    }

    // Spawn chunked isometric tile grid (-8..=8) with bitmask road auto-tiling and sidewalks
    let half_grid: i32 = 8;
    let grid_span = (half_grid * 2 + 1) as u32;
    let map_size = TilemapSize {
        x: grid_span,
        y: grid_span,
    };
    let mut tile_storage = TileStorage::empty(map_size);
    let tilemap_entity = commands
        .spawn((
            Name::new("Isometric Chunked TileStorage"),
            SiteScoped { epoch: *epoch },
            Transform::default(),
            GlobalTransform::default(),
        ))
        .id();

    for gy in -half_grid..=half_grid {
        for gx in -half_grid..=half_grid {
            let is_road_x = gx.abs() == 4 || gx == 0;
            let is_road_y = gy.abs() == 4 || gy == 0;
            let is_road = is_road_x || is_road_y;
            let is_sidewalk =
                !is_road && (gx.abs() == 3 || gx.abs() == 5 || gy.abs() == 3 || gy.abs() == 5);

            let (kind, raw_atlas_idx, road_mask, color) = if is_road {
                let n = gy < half_grid && (is_road_x || (gy + 1).abs() == 4 || (gy + 1) == 0);
                let s = gy > -half_grid && (is_road_x || (gy - 1).abs() == 4 || (gy - 1) == 0);
                let e = gx < half_grid && (is_road_y || (gx + 1).abs() == 4 || (gx + 1) == 0);
                let w = gx > -half_grid && (is_road_y || (gx - 1).abs() == 4 || (gx - 1) == 0);
                let mask = compute_road_bitmask(n, e, s, w);
                (
                    TerrainTileKind::RoadTwoLane,
                    u32::from(mask),
                    mask,
                    Color::srgb(0.23, 0.24, 0.26),
                )
            } else if is_sidewalk {
                (
                    TerrainTileKind::Sidewalk,
                    atlas_indices::TERRAIN_SIDEWALK,
                    0,
                    Color::srgb(0.66, 0.65, 0.62),
                )
            } else {
                let checker = ((gx + gy).rem_euclid(2)) == 0;
                let tint = if checker {
                    palette.grass_tint
                } else {
                    scale_srgb_color(palette.grass_tint, 0.94, 1.0)
                };
                (
                    TerrainTileKind::Grass,
                    atlas_indices::TERRAIN_GRASS,
                    0,
                    tint,
                )
            };

            let valid_idx = validate_atlas_index(raw_atlas_idx, MAX_PROCEDURAL_ATLAS_INDEX);
            let tile_center_world = Vec2::new(
                (gx as f32 + 0.5) * WORLD_TILE_SIZE,
                (gy as f32 + 0.5) * WORLD_TILE_SIZE,
            );
            let screen = world_to_iso_screen(tile_center_world, 0.0);
            let z = iso_depth_key(tile_center_world, 0.0, ISO_BIAS_GROUND_TILES);

            let tile_pos = TilePos {
                x: (gx + half_grid) as u32,
                y: (gy + half_grid) as u32,
            };
            let mut tile_cmds = commands.spawn((
                Name::new(format!("IsoTile ({gx},{gy})")),
                SiteScoped { epoch: *epoch },
                kind,
                tile_pos,
                TilemapId(tilemap_entity),
                TileTextureIndex(valid_idx),
                WorldPose::from_xy(tile_center_world.x, tile_center_world.y),
                IsoLayerBias(ISO_BIAS_GROUND_TILES),
                Transform::from_xyz(screen.x, screen.y, z),
                GlobalTransform::default(),
            ));

            if let Some(meshes_mut) = meshes.as_deref_mut() {
                let tile_mesh = build_site_tile_iso_mesh(gx, gy, kind, road_mask, color);
                tile_cmds.insert(Mesh2d(meshes_mut.add(tile_mesh)));
                if let Some(mat) = &mat_handle {
                    tile_cmds.insert(MeshMaterial2d(mat.clone()));
                }
            }

            let tile_entity = tile_cmds.id();
            tile_storage.set(&tile_pos, tile_entity);
        }
    }

    if let Some(mut tm_cmds) = commands.get_entity(tilemap_entity) {
        tm_cmds.insert(tile_storage);
    }
}

/// Processes `DecalSpawnRequest` events (mud tread tracks, oil slicks, scorch marks, crushed rubble
/// scars) using the 300-slot `GroundDecalPool` ring buffer and true oriented isometric ground meshes.
pub fn process_ground_decal_requests_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    mut decal_events: EventReader<DecalSpawnRequest>,
    mut decal_pool: ResMut<GroundDecalPool>,
    mut telemetry: ResMut<pd_core::DebugTelemetry>,
    mut shared_mat: ResMut<SharedIsoColorMaterial>,
    mut meshes: Option<ResMut<Assets<Mesh>>>,
    mut materials: Option<ResMut<Assets<ColorMaterial>>>,
) {
    let mat_handle = materials
        .as_deref_mut()
        .map(|mats| get_or_init_shared_iso_material(&mut shared_mat, mats));

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

        let screen = world_to_iso_screen(safe_pos, 0.0);
        let z = iso_depth_key(safe_pos, 0.0, ISO_BIAS_GROUND_DECALS);
        let mut decal_cmds = commands.spawn((
            Name::new("Dynamic Ground Decal"),
            SiteScoped { epoch: *epoch },
            GroundDecalScar {
                kind: TerrainTileKind::RubbleScar,
            },
            WorldPose::new(safe_pos, 0.0, safe_rot),
            IsoLayerBias(ISO_BIAS_GROUND_DECALS),
            Transform::from_xyz(screen.x, screen.y, z),
            GlobalTransform::default(),
        ));

        if let Some(meshes_mut) = meshes.as_deref_mut() {
            let decal_mesh = build_oriented_ground_mesh(
                safe_pos.x,
                safe_pos.y,
                safe_rot,
                safe_size.x,
                safe_size.y,
                0.0,
                screen,
                color,
                req.kind == DecalKind::MudTreadTrack,
            );
            decal_cmds.insert(Mesh2d(meshes_mut.add(decal_mesh)));
            if let Some(mat) = &mat_handle {
                decal_cmds.insert(MeshMaterial2d(mat.clone()));
            }
        }

        let decal_entity = decal_cmds.id();
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
    site_seed: Option<Res<SiteSeed>>,
    env_config: Res<EnvironmentConfig>,
    mut burst_events: EventReader<ParticleBurstEvent>,
    mut particle_pool: ResMut<WorldParticlePool>,
    mut particles: Query<(Entity, &mut WorldParticle, &mut WorldPose)>,
) {
    let Ok(dt) = sanitize_dt(time.delta_secs()) else {
        return;
    };

    // 1. Advance existing particles in Cartesian world space and despawn expired ones.
    for (entity, mut p, mut pose) in &mut particles {
        p.remaining_secs -= dt;
        let vel = sanitize_vec2(p.velocity, Vec2::ZERO);
        pose.pos.x = sanitize_f32(pose.pos.x + vel.x * dt, pose.pos.x);
        pose.pos.y = sanitize_f32(pose.pos.y + vel.y * dt, pose.pos.y);

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
                        dir * 95.0 + Vec2::new(-25.0, -25.0),
                        Color::srgba(0.35, 0.78, 0.98, 0.88),
                        Vec2::new(4.5, 4.5),
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
                        Vec2::new(10.0, 10.0),
                        0.55,
                    );
                }
                ParticleBurstKind::DieselExhaustSmoke => {
                    spawn_pooled_world_particle(
                        &mut commands,
                        &mut particle_pool,
                        *epoch,
                        base_pos,
                        Vec2::new(-28.0, -28.0) + dir * 12.0,
                        Color::srgba(0.22, 0.22, 0.24, 0.65),
                        Vec2::new(7.5, 7.5),
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
                        Vec2::new(7.0, 7.0),
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

        let seed = site_seed.map_or(0, |s| s.0);
        for _ in 0..to_spawn {
            // Seeded from SiteSeed + spawn serial (not wall-clock time) so captures reproduce.
            let serial = particle_pool.weather_spawn_serial;
            particle_pool.weather_spawn_serial = serial.wrapping_add(1);
            let mut rng = SplitMix64::new(seed ^ SplitMix64::mix(serial));
            let hash_x = (rng.next_unit_f32() * 2.0 - 1.0) * 260.0;
            let hash_y = (rng.next_unit_f32() * 2.0 - 1.0) * 260.0;
            let spawn_pos = Vec2::new(sanitize_f32(hash_x, 0.0), sanitize_f32(hash_y, 0.0));
            match env_config.resolved_season {
                Season::Spring | Season::Summer | Season::Random => {
                    spawn_pooled_world_particle(
                        &mut commands,
                        &mut particle_pool,
                        *epoch,
                        spawn_pos,
                        Vec2::new(90.0, 90.0),
                        Color::srgba(0.62, 0.80, 0.96, 0.65),
                        Vec2::new(2.0, 8.0),
                        1.4,
                    );
                }
                Season::Autumn => {
                    spawn_pooled_world_particle(
                        &mut commands,
                        &mut particle_pool,
                        *epoch,
                        spawn_pos,
                        Vec2::new(45.0, 30.0),
                        Color::srgba(0.88, 0.44, 0.14, 0.85),
                        Vec2::new(5.5, 4.0),
                        2.5,
                    );
                }
                Season::Winter => {
                    spawn_pooled_world_particle(
                        &mut commands,
                        &mut particle_pool,
                        *epoch,
                        spawn_pos,
                        Vec2::new(35.0, 35.0),
                        Color::srgba(0.95, 0.97, 1.0, 0.88),
                        Vec2::new(4.5, 4.5),
                        2.4,
                    );
                }
            }
        }
    }
}

/// Syncs Cartesian `WorldPose` (and optional `IsoLayerBias`) into Bevy's render `Transform`
/// (`world_to_iso_screen` and `iso_depth_key`) without mutating simulation state.
pub fn project_world_poses_to_isometric_transform_system(
    mut query: Query<(&WorldPose, Option<&IsoLayerBias>, &mut Transform)>,
) {
    for (pose, bias, mut transform) in &mut query {
        let safe_pose = pose.sanitized();
        let screen = world_to_iso_screen(safe_pose.pos, safe_pose.elev_z);
        let layer_bias = bias.map_or(0.0, |b| b.0);
        let z = iso_depth_key(safe_pose.pos, safe_pose.elev_z, layer_bias);

        transform.translation = Vec3::new(screen.x, screen.y, z);
        transform.rotation = Quat::IDENTITY;
    }
}

/// Generates or updates 2.5D extruded isometric `Mesh2d` assets for all entities carrying
/// `IsoExtrudedBox` (and optional `IsoBoxAssembly` / `WorldPose`). Reuses mesh attribute/index
/// buffers and `Local<IsoMeshBuildScratch>` across rebuilds, removes stale `Aabb` bounds so Bevy
/// recomputes accurate culling bounds, and safely no-ops in headless `MinimalPlugins` tests where
/// `Assets<Mesh>` is absent.
pub fn sync_iso_extruded_box_meshes_system(
    mut commands: Commands,
    mut shared_mat: ResMut<SharedIsoColorMaterial>,
    mut scratch: Local<IsoMeshBuildScratch>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    mut materials: Option<ResMut<Assets<ColorMaterial>>>,
    query: Query<(
        Entity,
        &IsoExtrudedBox,
        Option<&WorldPose>,
        Option<&IsoBoxAssembly>,
        Option<&Mesh2d>,
        Option<&IsoMeshCacheKey>,
    )>,
) {
    let Some(mut meshes) = meshes else {
        return;
    };
    let mat_handle = materials
        .as_deref_mut()
        .map(|mats| get_or_init_shared_iso_material(&mut shared_mat, mats));

    for (entity, root_box, maybe_pose, maybe_assembly, maybe_mesh2d, maybe_cache) in &query {
        let pose = maybe_pose.copied().unwrap_or_default().sanitized();
        let new_key = IsoMeshCacheKey::compute(&pose, root_box, maybe_assembly);

        if let (Some(_), Some(old_key)) = (maybe_mesh2d, maybe_cache)
            && *old_key == new_key
        {
            continue;
        }

        if let Some(existing_mesh2d) = maybe_mesh2d
            && let Some(mesh_asset) = meshes.get_mut(&existing_mesh2d.0)
        {
            update_iso_extruded_box_mesh_in_place(
                mesh_asset,
                &mut scratch,
                &pose,
                root_box,
                maybe_assembly,
            );
            if let Some(mut e_cmds) = commands.get_entity(entity) {
                e_cmds.remove::<Aabb>().insert(new_key);
            }
        } else if let Some(mut e_cmds) = commands.get_entity(entity) {
            let built_mesh = build_iso_extruded_box_mesh_with_scratch(
                &mut scratch,
                &pose,
                root_box,
                maybe_assembly,
            );
            let handle = meshes.add(built_mesh);
            e_cmds.remove::<Aabb>().insert((Mesh2d(handle), new_key));
            if let Some(mat) = &mat_handle {
                e_cmds.insert(MeshMaterial2d(mat.clone()));
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
            .init_resource::<PileField>()
            .init_resource::<SharedIsoColorMaterial>()
            .add_systems(Startup, init_procedural_world_atlas_system)
            .add_systems(
                Update,
                (
                    sync_world_on_site_restart_system,
                    ensure_site_ground_spawned_system.run_if(in_state(AppState::Playing)),
                    process_ground_decal_requests_system.run_if(in_state(AppState::Playing)),
                    update_weather_and_burst_particles_system.run_if(in_state(AppState::Playing)),
                    project_world_poses_to_isometric_transform_system,
                    sync_iso_extruded_box_meshes_system,
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
    fn test_pile_field_mass_conservation_resistance_and_blade_push() {
        let mut pile = PileField::default();
        assert!(pile.total_mass() <= 1e-6);

        let drop_pos = Vec2::new(-120.0, 45.0);
        pile.add_mass(drop_pos, 18.5, MaterialKind::Brick);
        assert!((pile.total_mass() - 18.5).abs() < 1e-3);
        assert!(pile.height_at(drop_pos) > 0.0);
        assert!(pile.pile_resistance_at(drop_pos) > 0.0);
        assert!(pile.active_mound_cells() >= 9);

        // Compacting increases compaction and lowers loose height while strictly preserving mass
        let h_before = pile.height_at(drop_pos);
        pile.compact_point(drop_pos, 0.5);
        assert!((pile.total_mass() - 18.5).abs() < 1e-3);
        assert!(pile.height_at(drop_pos) < h_before);

        // Pushing pile with lowered blade transfers mass forward while conserving total mass
        let forward_pos = drop_pos + Vec2::new(24.0, 0.0);
        let forward_h_before = pile.height_at(forward_pos);
        pile.push_pile_with_blade(drop_pos, Vec2::X, 20.0, 6.0);
        assert!((pile.total_mass() - 18.5).abs() < 1e-3);
        assert!(pile.height_at(forward_pos) > forward_h_before);

        // Non-finite inputs are safely ignored without corrupting total mass
        pile.add_mass(Vec2::splat(f32::NAN), f32::NAN, MaterialKind::Concrete);
        assert!((pile.total_mass() - 18.5).abs() < 1e-3);

        pile.clear();
        assert!(pile.total_mass() <= 1e-6);
    }

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

    #[test]
    fn test_iso_quad_and_oriented_ground_vertices() {
        let [v0, v1, v2, v3] = iso_quad_vertices(0.0, 0.0, 32.0, 32.0, 0.0);
        // True 2:1 diamond: horizontal span is 64 (-32..+32), vertical span is 32 (0..-32)
        let width_span = v1.x - v3.x;
        let height_span = v0.y - v2.y;
        assert!((width_span - 64.0).abs() < 1e-4);
        assert!((height_span - 32.0).abs() < 1e-4);

        let oriented = oriented_ground_vertices(10.0, -20.0, 0.75, 24.0, 12.0, 0.0);
        for v in oriented {
            assert!(v.is_finite());
        }
    }

    #[test]
    fn test_upper_west_sun_lighting_and_extruded_box_mesh() {
        let base = Color::srgb(0.75, 0.55, 0.25);
        let top_lit = compute_face_lighting(base, Vec3::Z, 1.0).to_srgba();
        let se_lit = compute_face_lighting(base, Vec3::X, 1.0).to_srgba();
        let sw_lit = compute_face_lighting(base, Vec3::Y, 1.0).to_srgba();

        // Top lid receives brightest upper-west sunlight, South-East is medium, South-West is darkest
        assert!(top_lit.red > se_lit.red);
        assert!(se_lit.red > sw_lit.red);

        let pose = WorldPose::new(Vec2::new(15.0, -30.0), 0.0, 0.4).with_pitch_roll(0.08, -0.05);
        let iso_box = IsoExtrudedBox::new(30.0, 20.0, 18.0, base)
            .with_shadow(true)
            .with_roof_slope(RoofSlopeSpec::Gable {
                eave_scale: 0.5,
                ridge_scale: 1.2,
            });
        let flat_box = IsoExtrudedBox::new(30.0, 20.0, 18.0, base).with_shadow(true);
        let gable_mesh = build_iso_extruded_box_mesh(&pose, &iso_box, None);
        let flat_mesh = build_iso_extruded_box_mesh(&pose, &flat_box, None);
        assert!(gable_mesh.attribute(Mesh::ATTRIBUTE_POSITION).is_some());
        assert!(gable_mesh.attribute(Mesh::ATTRIBUTE_COLOR).is_some());
        assert!(gable_mesh.indices().is_some());

        // Gable roof adds two roof slope quads + two triangular end-caps (6 extra vertices over flat lid)
        let gable_verts = gable_mesh.count_vertices();
        let flat_verts = flat_mesh.count_vertices();
        assert_eq!(gable_verts, flat_verts + 10);
    }

    #[test]
    fn test_overlapping_assembly_box_depth_ordering_hull_vs_cab() {
        let pose = WorldPose::new(Vec2::ZERO, 0.0, 0.0);
        let hull = IsoExtrudedBox::new(36.0, 22.0, 10.0, Color::srgb(0.84, 0.66, 0.18))
            .with_offset(Vec2::ZERO, 3.0)
            .with_shadow(true);
        let cab = IsoExtrudedBox::new(14.0, 18.0, 16.0, Color::srgb(0.84, 0.66, 0.18))
            .with_offset(Vec2::new(-6.0, 0.0), 15.0)
            .with_shadow(false);
        let assembly = IsoBoxAssembly::new(vec![cab]);

        // At screen point (-5, 20), both hull and cab are intersected by the camera ray, and
        // cab's exit Z along the ray is higher (nearer to the camera) than hull's exit Z.
        let hull_entry = OrderedBoxEntry::from_box(&pose, hull);
        let cab_entry = OrderedBoxEntry::from_box(&pose, cab);
        let hull_ray = hull_entry.ray_interval_at_screen_point(Vec2::new(-5.0, 20.0));
        let cab_ray = cab_entry.ray_interval_at_screen_point(Vec2::new(-5.0, 20.0));
        assert!(hull_ray.is_some());
        assert!(cab_ray.is_some());
        if let (Some((_, h_exit)), Some((c_enter, c_exit))) = (hull_ray, cab_ray) {
            assert!(c_enter > h_exit);
            assert!(c_exit > h_exit);
        }

        let mut ordered = Vec::new();
        order_assembly_boxes_for_pose(&pose, &hull, Some(&assembly), &mut ordered);
        assert_eq!(ordered.len(), 2);
        // Hull (z0 = 3.0) must be ordered before Cab (z0 = 15.0) so Cab draws over Hull
        assert!((ordered.first().map_or(0.0, |e| e.spec.z0) - 3.0).abs() < 1e-4);
        assert!((ordered.get(1).map_or(0.0, |e| e.spec.z0) - 15.0).abs() < 1e-4);
    }

    #[test]
    fn test_iso_mesh_cache_key_covers_all_geometry_and_style_inputs() {
        let pose = WorldPose::new(Vec2::ZERO, 0.0, 0.0);
        let base = IsoExtrudedBox::new(20.0, 14.0, 10.0, Color::srgb(0.5, 0.5, 0.5));
        let part = IsoExtrudedBox::new(8.0, 6.0, 4.0, Color::srgb(0.7, 0.7, 0.7))
            .with_offset(Vec2::new(2.0, 1.0), 10.0);
        let asm = IsoBoxAssembly::new(vec![part]);
        let baseline = IsoMeshCacheKey::compute(&pose, &base, Some(&asm));

        // Changing length or width invalidates cache key
        let diff_len = IsoExtrudedBox {
            length: 24.0,
            ..base
        };
        assert_ne!(
            baseline,
            IsoMeshCacheKey::compute(&pose, &diff_len, Some(&asm))
        );

        let diff_wid = IsoExtrudedBox {
            width: 18.0,
            ..base
        };
        assert_ne!(
            baseline,
            IsoMeshCacheKey::compute(&pose, &diff_wid, Some(&asm))
        );

        // Changing roof slope invalidates cache key
        let diff_slope = base.with_roof_slope(RoofSlopeSpec::Gable {
            eave_scale: 0.7,
            ridge_scale: 1.2,
        });
        assert_ne!(
            baseline,
            IsoMeshCacheKey::compute(&pose, &diff_slope, Some(&asm))
        );

        // Changing shadow flag invalidates cache key
        let diff_shadow = base.with_shadow(true);
        assert_ne!(
            baseline,
            IsoMeshCacheKey::compute(&pose, &diff_shadow, Some(&asm))
        );

        // Changing assembly part rotation or alpha invalidates cache key
        let rot_part = part.with_heading(0.35);
        let rot_asm = IsoBoxAssembly::new(vec![rot_part]);
        assert_ne!(
            baseline,
            IsoMeshCacheKey::compute(&pose, &base, Some(&rot_asm))
        );

        let alpha_part = part.with_alpha(0.4);
        let alpha_asm = IsoBoxAssembly::new(vec![alpha_part]);
        assert_ne!(
            baseline,
            IsoMeshCacheKey::compute(&pose, &base, Some(&alpha_asm))
        );
    }

    #[test]
    fn test_sync_iso_meshes_reuses_buffers_and_clears_cached_aabb() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<ColorMaterial>()
            .init_resource::<SharedIsoColorMaterial>()
            .add_systems(Update, sync_iso_extruded_box_meshes_system);

        let entity = app
            .world_mut()
            .spawn((
                WorldPose::new(Vec2::ZERO, 0.0, 0.0),
                IsoExtrudedBox::new(20.0, 12.0, 8.0, Color::srgb(0.8, 0.6, 0.2)),
                Aabb::default(),
            ))
            .id();

        app.update();
        assert!(app.world().entity(entity).get::<Mesh2d>().is_some());
        assert!(app.world().entity(entity).get::<Aabb>().is_none());

        // Re-insert a cached Aabb, rotate WorldPose to trigger an in-place mesh rebuild, and
        // verify the stale Aabb is removed on rebuild.
        app.world_mut().entity_mut(entity).insert(Aabb::default());
        if let Some(mut pose) = app.world_mut().entity_mut(entity).get_mut::<WorldPose>() {
            pose.heading_rad = 0.85;
        }
        app.update();
        assert!(app.world().entity(entity).get::<Aabb>().is_none());
    }
}
