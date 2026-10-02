use avian3d::prelude::*;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

// --- Structural & Building Components (per GDD section 4.2) ---

#[derive(Component, Debug, Clone)]
#[allow(dead_code)]
pub struct BuildingRoot {
    pub building_id: String,
    pub cell_dimensions: Vec3,
}

#[derive(Component, Debug, Clone)]
#[allow(dead_code)]
pub struct StructuralCell {
    pub building_id: String,
    pub grid_coord: IVec3,
    pub max_impulse_resistance: f32,
    pub is_anchor: bool,
    pub connected_neighbors: u8, // Bitflags for 6 directions
    pub material_name: String,
    pub density: f32,
}

#[derive(Component, Debug, Clone)]
pub struct RubbleState {
    pub stationary_timer: f32,
    pub can_bake: bool,
}

impl Default for RubbleState {
    fn default() -> Self {
        Self {
            stationary_timer: 0.0,
            can_bake: false,
        }
    }
}

// Direction bitflags
pub const DIR_POS_X: u8 = 1 << 0; // +X (0)
pub const DIR_NEG_X: u8 = 1 << 1; // -X (1)
pub const DIR_POS_Y: u8 = 1 << 2; // +Y (2)
pub const DIR_NEG_Y: u8 = 1 << 3; // -Y (3)
pub const DIR_POS_Z: u8 = 1 << 4; // +Z (4)
pub const DIR_NEG_Z: u8 = 1 << 5; // -Z (5)

pub const ALL_DIRS: [(IVec3, u8, u8); 6] = [
    (IVec3::new(1, 0, 0), DIR_POS_X, DIR_NEG_X),
    (IVec3::new(-1, 0, 0), DIR_NEG_X, DIR_POS_X),
    (IVec3::new(0, 1, 0), DIR_POS_Y, DIR_NEG_Y),
    (IVec3::new(0, -1, 0), DIR_NEG_Y, DIR_POS_Y),
    (IVec3::new(0, 0, 1), DIR_POS_Z, DIR_NEG_Z),
    (IVec3::new(0, 0, -1), DIR_NEG_Z, DIR_POS_Z),
];

// --- RON Schema Specification (per GDD section 5) ---

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MaterialSpec {
    pub yield_threshold: f32,
    pub density: f32,
    pub color: String,
    pub drop_on_collapse: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LayerSpec {
    pub y_index: usize,
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BuildingSpec {
    pub name: String,
    pub version: u32,
    pub cell_dimensions: (f32, f32, f32),
    pub materials: HashMap<String, MaterialSpec>,
    pub layers: Vec<LayerSpec>,
}

#[derive(Resource)]
pub struct BfsTimer {
    pub timer: Timer,
}

impl Default for BfsTimer {
    fn default() -> Self {
        // Runs at 10 Hz (0.10s)
        Self {
            timer: Timer::from_seconds(0.1, TimerMode::Repeating),
        }
    }
}

// Event triggered when a building should be reloaded
#[derive(Event, Default)]
pub struct ReloadBuildingEvent;

// Resource tracking building statistics for HUD
#[derive(Resource, Default)]
pub struct BuildingStats {
    pub total_cells: usize,
    pub intact_cells: usize,
    pub fractured_cells: usize,
    pub roof_intact_pct: f32,
}

// --- Spawn Building From Spec ---

pub fn spawn_building_from_spec(
    commands: &mut Commands,
    spec: &BuildingSpec,
    base_origin: Vec3,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) -> Entity {
    let (dim_x, dim_y, dim_z) = spec.cell_dimensions;
    let cell_size = Vec3::new(dim_x, dim_y, dim_z);
    let cell_mesh = meshes.add(Cuboid::new(dim_x, dim_y, dim_z));

    // Prepare material asset handles
    let mut material_handles: HashMap<String, Handle<StandardMaterial>> = HashMap::new();
    for (mat_name, mat_spec) in &spec.materials {
        let color = parse_hex_color(&mat_spec.color);
        let alpha = if mat_name.contains("glass") { 0.55 } else { 1.0 };
        let standard_mat = StandardMaterial {
            base_color: color.with_alpha(alpha),
            metallic: if mat_name.contains("pillar") { 0.4 } else { 0.05 },
            perceptual_roughness: if mat_name.contains("glass") { 0.1 } else { 0.8 },
            reflectance: if mat_name.contains("glass") { 0.9 } else { 0.2 },
            alpha_mode: if alpha < 1.0 {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            },
            ..default()
        };
        material_handles.insert(mat_name.clone(), materials.add(standard_mat));
    }

    let building_root_entity = commands
        .spawn((
            BuildingRoot {
                building_id: spec.name.clone(),
                cell_dimensions: cell_size,
            },
            Transform::from_translation(base_origin),
            Visibility::default(),
        ))
        .id();

    // Map grid coord -> cell data to assign neighbor connections
    let mut grid: HashMap<IVec3, (String, MaterialSpec)> = HashMap::new();

    for layer in &spec.layers {
        let y = layer.y_index as i32;
        let num_rows = layer.rows.len();
        for (r_idx, row) in layer.rows.iter().enumerate() {
            let z = (num_rows as i32 / 2) - r_idx as i32;
            let num_cols = row.len();
            for (c_idx, cell_mat) in row.iter().enumerate() {
                if cell_mat == "air" || cell_mat.is_empty() {
                    continue;
                }
                let x = c_idx as i32 - (num_cols as i32 / 2);
                let coord = IVec3::new(x, y, z);
                if let Some(mat_spec) = spec.materials.get(cell_mat) {
                    grid.insert(coord, (cell_mat.clone(), mat_spec.clone()));
                }
            }
        }
    }

    // Now spawn cells and compute initial 6-directional connectivity
    for (&coord, (mat_name, mat_spec)) in &grid {
        let mut neighbors_mask = 0u8;
        for &(dir_offset, forward_flag, _) in &ALL_DIRS {
            let neighbor_coord = coord + dir_offset;
            if grid.contains_key(&neighbor_coord) {
                neighbors_mask |= forward_flag;
            }
        }

        let is_anchor = coord.y == 0;
        let world_pos = base_origin
            + Vec3::new(
                coord.x as f32 * dim_x,
                coord.y as f32 * dim_y + dim_y * 0.5,
                coord.z as f32 * dim_z,
            );

        let cell_mat_handle = material_handles
            .get(mat_name)
            .cloned()
            .unwrap_or_default();

        commands.spawn((
            StructuralCell {
                building_id: spec.name.clone(),
                grid_coord: coord,
                max_impulse_resistance: mat_spec.yield_threshold,
                is_anchor,
                connected_neighbors: neighbors_mask,
                material_name: mat_name.clone(),
                density: mat_spec.density,
            },
            // Fixed anchor / static intact wall
            RigidBody::Static,
            Collider::cuboid(dim_x, dim_y, dim_z),
            Mesh3d(cell_mesh.clone()),
            MeshMaterial3d(cell_mat_handle),
            Transform::from_translation(world_pos),
        ));
    }

    building_root_entity
}

// --- FixedUpdate (10 Hz): structural_integrity_bfs_system ---

pub fn structural_integrity_bfs_system(
    time: Res<Time>,
    mut bfs_timer: ResMut<BfsTimer>,
    mut commands: Commands,
    mut stats: ResMut<BuildingStats>,
    cell_query: Query<(Entity, &StructuralCell, &Transform, &RigidBody)>,
) {
    bfs_timer.timer.tick(time.delta());
    if !bfs_timer.timer.just_finished() {
        return;
    }

    // Map intact static cells by grid coordinate
    let mut intact_cells: HashMap<IVec3, (Entity, u8, bool, f32, Vec3)> = HashMap::new();
    let mut total_count = 0;
    let mut roof_total = 0;
    let mut roof_intact = 0;

    for (entity, cell, transform, rb) in cell_query.iter() {
        total_count += 1;
        if cell.grid_coord.y >= 1 {
            roof_total += 1;
        }

        if *rb == RigidBody::Static {
            intact_cells.insert(
                cell.grid_coord,
                (
                    entity,
                    cell.connected_neighbors,
                    cell.is_anchor,
                    cell.density,
                    transform.translation,
                ),
            );

            if cell.grid_coord.y >= 1 {
                roof_intact += 1;
            }
        }
    }

    stats.total_cells = total_count;
    stats.intact_cells = intact_cells.len();
    stats.fractured_cells = total_count.saturating_sub(intact_cells.len());
    stats.roof_intact_pct = if roof_total > 0 {
        (roof_intact as f32 / roof_total as f32) * 100.0
    } else {
        100.0
    };

    if intact_cells.is_empty() {
        return;
    }

    // 1. Find all active ground anchors (cells with is_anchor == true and intact)
    let mut bfs_queue: VecDeque<IVec3> = VecDeque::new();
    let mut visited: HashSet<IVec3> = HashSet::new();

    for (&coord, &(_, _, is_anchor, _, _)) in &intact_cells {
        if is_anchor {
            bfs_queue.push_back(coord);
            visited.insert(coord);
        }
    }

    // 2. BFS graph reachability
    while let Some(current_coord) = bfs_queue.pop_front() {
        let Some(&(_, current_neighbors, _, _, _)) = intact_cells.get(&current_coord) else {
            continue;
        };

        for &(offset, forward_flag, opposite_flag) in &ALL_DIRS {
            // Is this cell connected in this direction?
            if (current_neighbors & forward_flag) != 0 {
                let neighbor_coord = current_coord + offset;
                if let Some(&(_, neighbor_mask, _, _, _)) = intact_cells.get(&neighbor_coord) {
                    // And does neighbor connect back?
                    if (neighbor_mask & opposite_flag) != 0 && !visited.contains(&neighbor_coord) {
                        visited.insert(neighbor_coord);
                        bfs_queue.push_back(neighbor_coord);
                    }
                }
            }
        }
    }

    // 3. Any segment disconnected from all ground anchors is tagged as Orphaned!
    let mut orphaned_entities: Vec<(Entity, IVec3, f32)> = Vec::new();
    for (&coord, &(entity, _, _, density, _)) in &intact_cells {
        if !visited.contains(&coord) {
            orphaned_entities.push((entity, coord, density));
        }
    }

    // 4. Orphaned segments immediately drop RigidBody::Static and become RigidBody::Dynamic
    for (entity, coord, density) in orphaned_entities {
        // Calculate mass = volume * density (default 1.2 x 1.0 x 1.2 = 1.44 m3)
        let volume = 1.44;
        let mass_val = (volume * density).max(100.0);

        commands
            .entity(entity)
            .remove::<RigidBody>()
            .remove::<StructuralCell>() // Disconnect from structural graph
            .insert((
                RigidBody::Dynamic,
                Mass(mass_val),
                LinearDamping(0.2),
                AngularDamping(0.3),
                RubbleState::default(),
            ));

        info!(
            "Structural collapse: orphaned block at {:?} fell dynamically (mass: {:.1} kg)",
            coord, mass_val
        );
    }
}

// --- Reload building handler ---

pub fn reload_building_system(
    mut commands: Commands,
    mut reload_events: EventReader<ReloadBuildingEvent>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    building_query: Query<Entity, With<BuildingRoot>>,
    cell_query: Query<Entity, Or<(With<StructuralCell>, With<RubbleState>)>>,
) {
    for _ in reload_events.read() {
        info!("Reloading building from RON schema...");

        // Despawn old building entities
        for entity in building_query.iter() {
            commands.entity(entity).despawn_recursive();
        }
        for entity in cell_query.iter() {
            commands.entity(entity).despawn();
        }

        // Load RON file
        let ron_path = "assets/buildings/sample_hardware_store.building.ron";
        if let Ok(content) = std::fs::read_to_string(ron_path) {
            match ron::from_str::<BuildingSpec>(&content) {
                Ok(spec) => {
                    let base_origin = Vec3::new(0.0, 0.0, -14.0);
                    spawn_building_from_spec(
                        &mut commands,
                        &spec,
                        base_origin,
                        &mut meshes,
                        &mut materials,
                    );
                    info!("Building '{}' loaded successfully!", spec.name);
                }
                Err(e) => {
                    error!("Failed to parse building RON: {:?}", e);
                }
            }
        } else {
            error!("Could not open RON building file: {}", ron_path);
        }
    }
}

// Color parsing utility for hex colors like "#8B3E2F"
fn parse_hex_color(hex: &str) -> Color {
    let clean_hex = hex.trim_start_matches('#');
    if clean_hex.len() >= 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&clean_hex[0..2], 16),
            u8::from_str_radix(&clean_hex[2..4], 16),
            u8::from_str_radix(&clean_hex[4..6], 16),
        ) {
            return Color::srgb(
                r as f32 / 255.0,
                g as f32 / 255.0,
                b as f32 / 255.0,
            );
        }
    }
    Color::srgb(0.7, 0.7, 0.7)
}
