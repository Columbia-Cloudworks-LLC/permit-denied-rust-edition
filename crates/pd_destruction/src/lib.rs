//! `pd_destruction`: Modular building structural graph, `.ron` blueprint loader, epoch-guarded
//! async BFS collapse tasks, 3-state damage visuals (`Intact`, `CrackedStressed`, `RubbleDebris`),
//! interior furniture & exterior props (with water bursts and propane explosions), and 3-tier
//! debris ring-buffer pooling (capped at 500 active dynamic physics bodies).
//!
//! Depends only on `pd_core` and `pd_world`.

use avian2d::prelude::*;
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on, futures_lite::future};
use pd_core::{
    ActiveCollapseZones, AppState, AudioSfxKind, AudioTriggerEvent, CampaignLevel, CollapseZone,
    DebugTelemetry, DemolitionCashEvent, EnvironmentConfig, IsoBoxAssembly, IsoExtrudedBox,
    IsoLayerBias, ParticleBurstEvent, ParticleBurstKind, PdSystemSet, RoofSlopeSpec, SessionEpoch,
    SiteRestartRequested, SiteScoped, WorldPose, iso_depth_key, sanitize_dt, sanitize_f32,
    sanitize_vec2, world_to_iso_screen,
};
use pd_world::{CutawayFootprint, ProceduralWorldAtlas, RoadSplineGraph};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Embedded `.ron` campaign level prefab definitions guaranteeing zero-panic loading.
pub const EMBEDDED_CAMPAIGN_PREFABS_RON: &str =
    include_str!("../../../assets/prefabs/campaign_levels.prefab.ron");

/// Hard ceiling on active dynamic physics debris bodies per the PRD performance guardrail.
pub const MAX_ACTIVE_PHYSICS_DEBRIS: usize = 500;

/// Hard iteration cap on Breadth-First Search (BFS) structural connectivity evaluation.
pub const MAX_GRAPH_ITERATIONS: usize = 4_096;

/// Strongly-typed node index within a building's structural graph.
pub type NodeIndex = u32;

/// Deserialized `.ron` level definition entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelPrefabEntry {
    pub id: u8,
    pub name: String,
    pub archetypes: Vec<String>,
    pub max_stories: u8,
    pub foundation_yield_impulse: f32,
}

/// Root `.ron` schema for `assets/prefabs/campaign_levels.prefab.ron`.
#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize)]
pub struct CampaignPrefabsManifest {
    pub levels: Vec<LevelPrefabEntry>,
}

impl CampaignPrefabsManifest {
    /// Parses the `.ron` campaign prefab manifest without panicking.
    #[must_use]
    pub fn load_embedded() -> Self {
        match ron::from_str::<Self>(EMBEDDED_CAMPAIGN_PREFABS_RON) {
            Ok(manifest) => manifest,
            Err(err) => {
                warn!("Failed to parse embedded .ron prefab manifest: {err:?}");
                Self::default()
            }
        }
    }

    #[must_use]
    pub fn entry_for_level(&self, level: CampaignLevel) -> Option<&LevelPrefabEntry> {
        let target_id = level.index_one_based();
        self.levels.iter().find(|e| e.id == target_id)
    }
}

/// Status returned by `queue_structural_integrity_evaluation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalStatus {
    Dispatched,
    Coalesced,
    Skipped,
}

/// Visual damage state for each modular structural block.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum DamageState {
    #[default]
    Intact,
    CrackedStressed,
    RubbleDebris,
}

/// Structural module archetype in `.ron` building prefabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum StructuralModuleKind {
    #[default]
    Wall,
    LoadBearingPillar,
    FloorSlab,
    RoofCorrugation,
}

/// Interior and exterior prop archetypes per Section 4 of the PRD.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PropArchetype {
    // Residential interior
    Couch,
    Bed,
    DiningTable,
    Television,
    Refrigerator,
    Bookshelf,
    Toilet,
    // Commercial interior
    GlassDisplayCase,
    CashRegister,
    MetalShelving,
    Forklift,
    IndustrialCrate,
    OfficeDesk,
    FilingCabinet,
    // Exterior props
    Mailbox,
    StreetSign,
    FireHydrant,
    Dumpster,
    PropaneTank,
    WoodenPallet,
    ConcreteBarrier,
}

impl PropArchetype {
    #[must_use]
    pub const fn cash_value(self) -> u32 {
        match self {
            Self::CashRegister => 89,
            Self::Forklift => 71,
            Self::PropaneTank => 65,
            Self::GlassDisplayCase | Self::Refrigerator => 45,
            Self::Television | Self::Dumpster | Self::FireHydrant => 32,
            Self::Couch | Self::Bed | Self::DiningTable | Self::Toilet => 16,
            Self::Bookshelf
            | Self::MetalShelving
            | Self::IndustrialCrate
            | Self::OfficeDesk
            | Self::FilingCabinet
            | Self::ConcreteBarrier => 12,
            Self::Mailbox | Self::StreetSign | Self::WoodenPallet => 5,
        }
    }

    #[must_use]
    pub const fn color(self) -> Color {
        match self {
            Self::CashRegister => Color::srgb(0.96, 0.82, 0.24),
            Self::PropaneTank | Self::FireHydrant => Color::srgb(0.92, 0.24, 0.18),
            Self::Toilet | Self::Refrigerator | Self::GlassDisplayCase => {
                Color::srgb(0.86, 0.92, 0.96)
            }
            Self::Forklift => Color::srgb(0.92, 0.64, 0.14),
            Self::Dumpster => Color::srgb(0.22, 0.48, 0.28),
            _ => Color::srgb(0.64, 0.46, 0.28),
        }
    }
}

/// Component attached to destructible interior furniture and exterior street props.
#[derive(Component, Debug, Clone, Copy)]
pub struct DestructibleProp {
    pub archetype: PropArchetype,
    pub is_interior: bool,
}

/// Single node in a building's modular structural graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralNode {
    pub id: NodeIndex,
    pub kind: StructuralModuleKind,
    pub grid_x: i32,
    pub grid_y: i32,
    pub story_level: u8,
    pub is_foundation: bool,
    pub intact: bool,
    pub damage_state: DamageState,
    pub cash_value: u32,
    pub neighbors: Vec<NodeIndex>,
}

/// Result payload produced by an async BFS connectivity evaluation task.
#[derive(Debug, Clone)]
pub struct CollapseEvalResult {
    pub building_id: Entity,
    pub epoch: SessionEpoch,
    pub generation: u64,
    pub orphaned_nodes: Vec<NodeIndex>,
    pub stressed_neighbors: Vec<NodeIndex>,
    pub iterations_used: usize,
}

/// Per-building structural graph state managed in `BuildingGraphRegistry`.
#[derive(Debug)]
pub struct BuildingGraphEntry {
    pub epoch: SessionEpoch,
    pub dirty_generation: u64,
    pub collapsing: bool,
    pub pending_recheck: bool,
    pub nodes: HashMap<NodeIndex, StructuralNode>,
    pub in_flight_task: Option<Task<CollapseEvalResult>>,
}

impl BuildingGraphEntry {
    #[must_use]
    pub fn new(epoch: SessionEpoch, nodes_list: Vec<StructuralNode>) -> Self {
        let mut nodes = HashMap::with_capacity(nodes_list.len());
        for node in nodes_list {
            nodes.insert(node.id, node);
        }
        Self {
            epoch,
            dirty_generation: 0,
            collapsing: false,
            pending_recheck: false,
            nodes,
            in_flight_task: None,
        }
    }
}

/// Resource indexing all active building structural graphs on the current site.
#[derive(Resource, Debug, Default)]
pub struct BuildingGraphRegistry {
    pub current_epoch: SessionEpoch,
    pub buildings: HashMap<Entity, BuildingGraphEntry>,
}

impl BuildingGraphRegistry {
    pub fn clear_for_epoch(&mut self, new_epoch: SessionEpoch) {
        self.current_epoch = new_epoch;
        self.buildings.clear();
    }
}

/// Pure bounded Breadth-First Search (BFS) that finds all intact nodes no longer connected
/// to any intact foundation node (`is_foundation == true` or `story_level == 0`).
/// Guaranteed to terminate within `MAX_GRAPH_ITERATIONS` (`4096`) steps.
#[must_use]
pub fn evaluate_orphaned_nodes_bounded(
    building_id: Entity,
    epoch: SessionEpoch,
    generation: u64,
    snapshot: &HashMap<NodeIndex, StructuralNode>,
    recently_destroyed: &[NodeIndex],
) -> CollapseEvalResult {
    let mut visited = std::collections::HashSet::with_capacity(snapshot.len());
    let mut queue = VecDeque::with_capacity(snapshot.len());

    // Seed BFS queue with all intact foundation nodes.
    for (&id, node) in snapshot {
        if node.intact && (node.is_foundation || node.story_level == 0) {
            visited.insert(id);
            queue.push_back(id);
        }
    }

    let mut iterations: usize = 0;
    while let Some(curr_id) = queue.pop_front() {
        if iterations >= MAX_GRAPH_ITERATIONS {
            break;
        }
        iterations = iterations.saturating_add(1);

        if let Some(curr_node) = snapshot.get(&curr_id) {
            for &neighbor_id in &curr_node.neighbors {
                if !visited.contains(&neighbor_id)
                    && let Some(neighbor_node) = snapshot.get(&neighbor_id)
                    && neighbor_node.intact
                {
                    visited.insert(neighbor_id);
                    queue.push_back(neighbor_id);
                }
            }
        }
    }

    let mut orphaned_nodes = Vec::new();
    let mut stressed_neighbors = Vec::new();

    // If the iteration cap wasn't exceeded prematurely, any unvisited intact node is orphaned.
    if iterations < MAX_GRAPH_ITERATIONS {
        for (&id, node) in snapshot {
            if node.intact && !visited.contains(&id) {
                orphaned_nodes.push(id);
            }
        }
    }

    // Identify intact nodes adjacent to recently destroyed nodes to apply CrackedStressed visuals.
    for &destroyed_id in recently_destroyed {
        if let Some(d_node) = snapshot.get(&destroyed_id) {
            for &n_id in &d_node.neighbors {
                if let Some(n_node) = snapshot.get(&n_id)
                    && n_node.intact
                    && visited.contains(&n_id)
                {
                    stressed_neighbors.push(n_id);
                }
            }
        }
    }

    orphaned_nodes.sort_unstable();
    stressed_neighbors.sort_unstable();
    stressed_neighbors.dedup();

    CollapseEvalResult {
        building_id,
        epoch,
        generation,
        orphaned_nodes,
        stressed_neighbors,
        iterations_used: iterations,
    }
}

/// Contract Function (Module C): Marks nodes as destroyed in a building's structural graph
/// and dispatches a bounded Breadth-First Search (BFS) task to `AsyncComputeTaskPool` to find
/// disconnected upper-story clusters that have lost connection to foundation nodes.
pub fn queue_structural_integrity_evaluation(
    building_id: Entity,
    destroyed_nodes: &[NodeIndex],
    graph_registry: &mut BuildingGraphRegistry,
    task_pool: &AsyncComputeTaskPool,
) -> EvalStatus {
    let epoch = graph_registry.current_epoch;

    // 1. Lookup building_id in graph_registry. If missing or already Collapsing, return Skipped.
    let Some(entry) = graph_registry.buildings.get_mut(&building_id) else {
        return EvalStatus::Skipped;
    };
    if entry.collapsing || entry.epoch != epoch {
        return EvalStatus::Skipped;
    }

    // 2. Remove/mark destroyed_nodes in the adjacency graph and increment dirty_generation.
    for &node_id in destroyed_nodes {
        if let Some(node) = entry.nodes.get_mut(&node_id) {
            node.intact = false;
            node.damage_state = DamageState::RubbleDebris;
        }
    }
    entry.dirty_generation = entry.dirty_generation.saturating_add(1);

    // 3. If an async BFS task is already in-flight for this building_id, coalesce into pending.
    if entry.in_flight_task.is_some() {
        entry.pending_recheck = true;
        return EvalStatus::Coalesced;
    }

    // 4. Dispatch bounded BFS on AsyncComputeTaskPool.
    let snapshot = entry.nodes.clone();
    let generation = entry.dirty_generation;
    let destroyed_vec = destroyed_nodes.to_vec();

    let task = task_pool.spawn(async move {
        evaluate_orphaned_nodes_bounded(building_id, epoch, generation, &snapshot, &destroyed_vec)
    });

    entry.in_flight_task = Some(task);
    entry.pending_recheck = false;
    EvalStatus::Dispatched
}

/// Lifecycle tier for the 3-Tier Debris Pooling system.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum DebrisTier {
    #[default]
    ActivePhysics,
    SleepingInstanced,
    CrushedDecal,
}

/// Component attached to spawned debris pieces tracking simulated 2.5D Z-height drop and settling.
#[derive(Component, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DebrisChunk {
    pub tier: DebrisTier,
    pub active_timer_secs: f32,
    pub simulated_z_height: f32,
    pub vertical_z_velocity: f32,
    pub horizontal_velocity: Vec2,
    pub spin_rate: f32,
}

/// Fixed-capacity ring-buffer pool governing active physics debris (`<= 500` bodies).
#[derive(Resource, Debug, Clone)]
pub struct DebrisPoolRegistry {
    pub active_ring: VecDeque<Entity>,
    pub max_capacity: usize,
    pub total_recycled: u64,
}

impl Default for DebrisPoolRegistry {
    fn default() -> Self {
        Self {
            active_ring: VecDeque::with_capacity(MAX_ACTIVE_PHYSICS_DEBRIS),
            max_capacity: MAX_ACTIVE_PHYSICS_DEBRIS,
            total_recycled: 0,
        }
    }
}

impl DebrisPoolRegistry {
    pub fn clear(&mut self) {
        self.active_ring.clear();
    }

    /// Evicts the oldest active physics debris entity into Tier 2 (`SleepingInstanced`) when the
    /// ring buffer reaches `max_capacity` (`500`), stripping its physics collider.
    pub fn register_or_recycle_oldest(
        &mut self,
        new_debris: Entity,
        commands: &mut Commands,
    ) -> Option<Entity> {
        let mut evicted = None;
        while self.active_ring.len() >= self.max_capacity {
            if let Some(oldest) = self.active_ring.pop_front() {
                self.total_recycled = self.total_recycled.saturating_add(1);
                if let Some(mut entity_cmds) = commands.get_entity(oldest) {
                    entity_cmds
                        .remove::<RigidBody>()
                        .remove::<Collider>()
                        .remove::<LinearVelocity>()
                        .remove::<AngularVelocity>()
                        .insert(DebrisTier::SleepingInstanced);
                }
                evicted = Some(oldest);
            } else {
                break;
            }
        }
        self.active_ring.push_back(new_debris);
        evicted
    }
}

/// Marker component for a modular building root entity.
#[derive(Component, Debug, Clone)]
pub struct ModularBuilding {
    pub archetype_name: String,
    pub stories: u8,
    pub palette_index: usize,
}

/// Helper that generates a multi-story modular building graph for the given campaign level.
#[must_use]
pub fn build_procedural_building_graph(stories: u8, footprint_half: i32) -> Vec<StructuralNode> {
    let clamped_stories = stories.clamp(1, 8);
    let half = footprint_half.clamp(1, 4);
    let mut nodes = Vec::new();
    let mut coord_to_id: HashMap<(i32, i32, u8), NodeIndex> = HashMap::new();

    let mut next_id: NodeIndex = 0;
    for story in 0..clamped_stories {
        for gy in -half..=half {
            for gx in -half..=half {
                let is_perimeter = gx.abs() == half || gy.abs() == half;
                let is_corner = gx.abs() == half && gy.abs() == half;
                let kind = if story == clamped_stories - 1 && !is_perimeter {
                    StructuralModuleKind::RoofCorrugation
                } else if is_corner {
                    StructuralModuleKind::LoadBearingPillar
                } else if is_perimeter {
                    StructuralModuleKind::Wall
                } else {
                    StructuralModuleKind::FloorSlab
                };

                let id = next_id;
                next_id = next_id.saturating_add(1);
                coord_to_id.insert((gx, gy, story), id);

                nodes.push(StructuralNode {
                    id,
                    kind,
                    grid_x: gx,
                    grid_y: gy,
                    story_level: story,
                    is_foundation: story == 0,
                    intact: true,
                    damage_state: DamageState::Intact,
                    cash_value: match kind {
                        StructuralModuleKind::LoadBearingPillar => 71,
                        StructuralModuleKind::Wall => 16,
                        StructuralModuleKind::FloorSlab | StructuralModuleKind::RoofCorrugation => {
                            5
                        }
                    },
                    neighbors: Vec::new(),
                });
            }
        }
    }

    // Connect 6-directional orthogonal neighbors (+/-x, +/-y, +/-story).
    let offsets: [(i32, i32, i8); 6] = [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ];

    for node in &mut nodes {
        for (dx, dy, dz) in offsets {
            let nx = node.grid_x.saturating_add(dx);
            let ny = node.grid_y.saturating_add(dy);
            let nz_i16 = i16::from(node.story_level) + i16::from(dz);
            if let Ok(nz) = u8::try_from(nz_i16)
                && let Some(&neighbor_id) = coord_to_id.get(&(nx, ny, nz))
            {
                node.neighbors.push(neighbor_id);
            }
        }
    }

    nodes
}

/// Builds a 2.5D multi-box isometric assembly for a modular building from its live
/// `StructuralNode` graph (`Issue #10`). Damaged/destroyed nodes are omitted or rendered
/// as cracked masonry so real-time structural breaches are visually carved out in 2.5D.
#[must_use]
pub fn build_building_iso_assembly(
    nodes: &HashMap<NodeIndex, StructuralNode>,
    stories: u8,
    palette_index: usize,
) -> (IsoExtrudedBox, IsoBoxAssembly) {
    let (wall_col, pillar_col, roof_col, trim_col) = match palette_index % 3 {
        0 => (
            Color::srgb(0.72, 0.36, 0.28),
            Color::srgb(0.54, 0.28, 0.22),
            Color::srgb(0.34, 0.36, 0.40),
            Color::srgb(0.84, 0.80, 0.72),
        ),
        1 => (
            Color::srgb(0.76, 0.68, 0.54),
            Color::srgb(0.58, 0.50, 0.40),
            Color::srgb(0.40, 0.34, 0.30),
            Color::srgb(0.88, 0.84, 0.76),
        ),
        _ => (
            Color::srgb(0.52, 0.58, 0.66),
            Color::srgb(0.36, 0.42, 0.48),
            Color::srgb(0.28, 0.32, 0.36),
            Color::srgb(0.74, 0.78, 0.82),
        ),
    };

    let base_slab = IsoExtrudedBox::new(112.0, 82.0, 3.0, Color::srgb(0.40, 0.38, 0.36))
        .with_offset(Vec2::ZERO, 0.0)
        .with_shadow(true);

    let mut sorted_ids: Vec<NodeIndex> = nodes.keys().copied().collect();
    sorted_ids.sort_unstable();

    let mut parts = Vec::with_capacity(sorted_ids.len() * 2 + 4);
    let story_h = 16.0_f32;

    for id in sorted_ids {
        let Some(node) = nodes.get(&id) else {
            continue;
        };
        if !node.intact {
            continue;
        }

        let local_xy = Vec2::new(node.grid_x as f32 * 21.0, node.grid_y as f32 * 15.0);
        let z0 = 3.0 + f32::from(node.story_level) * story_h;
        let is_stressed = node.damage_state == DamageState::CrackedStressed;

        let tint = |c: Color| -> Color {
            if is_stressed {
                let s = c.to_srgba();
                Color::srgba(s.red * 0.68, s.green * 0.64, s.blue * 0.60, s.alpha)
            } else {
                c
            }
        };

        match node.kind {
            StructuralModuleKind::LoadBearingPillar => {
                parts.push(
                    IsoExtrudedBox::new(20.0, 14.5, story_h, tint(pillar_col))
                        .with_offset(local_xy, z0)
                        .with_shadow(false),
                );
                // Cornice band at top of each story pillar
                parts.push(
                    IsoExtrudedBox::new(21.5, 15.5, 2.0, tint(trim_col))
                        .with_offset(local_xy, z0 + story_h - 2.0)
                        .with_shadow(false),
                );
            }
            StructuralModuleKind::Wall => {
                parts.push(
                    IsoExtrudedBox::new(21.0, 15.0, story_h, tint(wall_col))
                        .with_offset(local_xy, z0)
                        .with_shadow(false),
                );
                // Recessed glazed window on exterior walls
                if (node.grid_x + node.grid_y).abs() % 2 == 1 {
                    let glass_col = if is_stressed {
                        Color::srgb(0.18, 0.22, 0.26)
                    } else {
                        Color::srgb(0.32, 0.50, 0.64)
                    };
                    parts.push(
                        IsoExtrudedBox::new(11.0, 9.0, story_h * 0.52, glass_col)
                            .with_offset(local_xy, z0 + story_h * 0.26)
                            .with_shadow(false),
                    );
                }
            }
            StructuralModuleKind::FloorSlab => {
                parts.push(
                    IsoExtrudedBox::new(21.2, 15.2, 2.8, tint(trim_col))
                        .with_offset(local_xy, z0)
                        .with_shadow(false),
                );
            }
            StructuralModuleKind::RoofCorrugation => {
                let mut roof_box = IsoExtrudedBox::new(21.4, 15.4, 5.5, tint(roof_col))
                    .with_offset(local_xy, z0 + story_h - 4.5)
                    .with_shadow(false);
                if stories <= 2 && node.grid_y == 0 {
                    roof_box = roof_box.with_roof_slope(RoofSlopeSpec::Gable {
                        eave_scale: 0.55,
                        ridge_scale: 1.25,
                    });
                }
                parts.push(roof_box);

                // Rooftop HVAC unit on center roof module
                if node.grid_x == 0 && node.grid_y == 0 {
                    parts.push(
                        IsoExtrudedBox::new(12.0, 10.0, 6.5, Color::srgb(0.62, 0.66, 0.70))
                            .with_offset(local_xy, z0 + story_h + 1.0)
                            .with_shadow(false),
                    );
                }
            }
        }
    }

    (
        base_slab,
        IsoBoxAssembly {
            parts,
            alpha_multiplier: 1.0,
        },
    )
}

/// Builds a 2.5D multi-box assembly for interior furniture and exterior street props (`Issue #10`).
#[must_use]
pub fn build_prop_iso_assembly(archetype: PropArchetype) -> (IsoExtrudedBox, IsoBoxAssembly) {
    let mut parts = Vec::with_capacity(6);
    let base = match archetype {
        PropArchetype::PropaneTank => {
            parts.push(
                IsoExtrudedBox::new(18.0, 10.0, 9.5, Color::srgb(0.92, 0.88, 0.86))
                    .with_offset(Vec2::ZERO, 3.0)
                    .with_shadow(false),
            );
            parts.push(
                IsoExtrudedBox::new(6.0, 10.4, 9.8, Color::srgb(0.88, 0.22, 0.18))
                    .with_offset(Vec2::ZERO, 3.0)
                    .with_shadow(false),
            );
            parts.push(
                IsoExtrudedBox::new(4.0, 4.0, 3.2, Color::srgb(0.78, 0.64, 0.22))
                    .with_offset(Vec2::new(0.0, 0.0), 12.5)
                    .with_shadow(false),
            );
            IsoExtrudedBox::new(14.0, 8.0, 3.0, Color::srgb(0.32, 0.32, 0.35))
        }
        PropArchetype::FireHydrant => {
            parts.push(
                IsoExtrudedBox::new(9.5, 5.5, 3.0, Color::srgb(0.78, 0.78, 0.82))
                    .with_offset(Vec2::ZERO, 6.5)
                    .with_shadow(false),
            );
            parts.push(
                IsoExtrudedBox::new(4.5, 4.5, 3.0, Color::srgb(0.92, 0.78, 0.22))
                    .with_offset(Vec2::ZERO, 11.5)
                    .with_shadow(false),
            );
            IsoExtrudedBox::new(6.5, 6.5, 11.5, Color::srgb(0.90, 0.20, 0.16))
        }
        PropArchetype::Dumpster => {
            parts.push(
                IsoExtrudedBox::new(23.0, 15.0, 2.2, Color::srgb(0.16, 0.17, 0.19))
                    .with_offset(Vec2::ZERO, 13.5)
                    .with_shadow(false),
            );
            IsoExtrudedBox::new(22.0, 14.0, 13.5, Color::srgb(0.20, 0.46, 0.26))
        }
        PropArchetype::CashRegister => {
            parts.push(
                IsoExtrudedBox::new(6.5, 8.0, 6.0, Color::srgb(0.96, 0.82, 0.24))
                    .with_offset(Vec2::new(-2.0, 0.0), 5.0)
                    .with_shadow(false),
            );
            IsoExtrudedBox::new(11.0, 10.0, 5.0, Color::srgb(0.76, 0.62, 0.18))
        }
        PropArchetype::Forklift => {
            // Mast & forks
            parts.push(
                IsoExtrudedBox::new(2.5, 10.0, 16.0, Color::srgb(0.22, 0.23, 0.25))
                    .with_offset(Vec2::new(8.0, 0.0), 1.0)
                    .with_shadow(false),
            );
            for side in [-1.0_f32, 1.0_f32] {
                parts.push(
                    IsoExtrudedBox::new(9.0, 1.8, 1.2, Color::srgb(0.32, 0.33, 0.36))
                        .with_offset(Vec2::new(12.5, side * 3.2), 1.0)
                        .with_shadow(false),
                );
            }
            // ROPS cage
            parts.push(
                IsoExtrudedBox::new(10.0, 10.0, 8.0, Color::srgb(0.26, 0.28, 0.30))
                    .with_offset(Vec2::new(-1.0, 0.0), 8.0)
                    .with_shadow(false),
            );
            IsoExtrudedBox::new(16.0, 11.0, 8.0, Color::srgb(0.92, 0.64, 0.14))
        }
        PropArchetype::Couch => {
            parts.push(
                IsoExtrudedBox::new(5.0, 18.0, 6.5, Color::srgb(0.58, 0.28, 0.22))
                    .with_offset(Vec2::new(-3.5, 0.0), 4.0)
                    .with_shadow(false),
            );
            IsoExtrudedBox::new(12.0, 18.0, 4.5, Color::srgb(0.68, 0.34, 0.26))
        }
        PropArchetype::Refrigerator => {
            parts.push(
                IsoExtrudedBox::new(10.2, 10.2, 5.5, Color::srgb(0.92, 0.95, 0.98))
                    .with_offset(Vec2::ZERO, 11.5)
                    .with_shadow(false),
            );
            IsoExtrudedBox::new(10.0, 10.0, 11.0, Color::srgb(0.84, 0.88, 0.92))
        }
        PropArchetype::Toilet => {
            parts.push(
                IsoExtrudedBox::new(4.5, 7.5, 7.5, Color::srgb(0.92, 0.95, 0.98))
                    .with_offset(Vec2::new(-3.0, 0.0), 5.0)
                    .with_shadow(false),
            );
            IsoExtrudedBox::new(9.5, 7.0, 5.5, Color::srgb(0.88, 0.92, 0.96))
        }
        PropArchetype::GlassDisplayCase => {
            parts.push(
                IsoExtrudedBox::new(16.0, 9.0, 6.5, Color::srgb(0.58, 0.82, 0.92))
                    .with_offset(Vec2::ZERO, 5.0)
                    .with_shadow(false),
            );
            IsoExtrudedBox::new(16.5, 9.5, 5.0, Color::srgb(0.46, 0.34, 0.22))
        }
        PropArchetype::ConcreteBarrier => {
            IsoExtrudedBox::new(22.0, 7.0, 9.5, Color::srgb(0.66, 0.65, 0.62))
                .with_roof_slope(RoofSlopeSpec::Gable {
                    eave_scale: 0.70,
                    ridge_scale: 1.15,
                })
        }
        _ => IsoExtrudedBox::new(12.0, 10.0, 9.0, archetype.color()),
    };

    (
        base,
        IsoBoxAssembly {
            parts,
            alpha_multiplier: 1.0,
        },
    )
}

/// Builds a 2.5D multi-box assembly for a falling structural masonry/rebar debris chunk (`Issue #10`).
#[must_use]
pub fn build_debris_iso_assembly(story_level: u8) -> (IsoExtrudedBox, IsoBoxAssembly) {
    let base_col = if story_level.is_multiple_of(2) {
        Color::srgb(0.56, 0.44, 0.36)
    } else {
        Color::srgb(0.48, 0.46, 0.44)
    };
    let base = IsoExtrudedBox::new(11.5, 8.5, 6.5, base_col).with_shadow(true);
    let parts = vec![
        IsoExtrudedBox::new(6.0, 5.0, 4.0, Color::srgb(0.36, 0.34, 0.32))
            .with_offset(Vec2::new(2.5, -1.5), 5.0)
            .with_shadow(false),
    ];
    (
        base,
        IsoBoxAssembly {
            parts,
            alpha_multiplier: 1.0,
        },
    )
}

/// Spawns a single dynamic debris entity in Cartesian `WorldPose` space and registers it
/// in the 500-cap ring buffer.
pub fn spawn_pooled_debris_chunk(
    commands: &mut Commands,
    debris_pool: &mut DebrisPoolRegistry,
    epoch: SessionEpoch,
    world_pos: Vec2,
    story_level: u8,
) -> Entity {
    let safe_pos = sanitize_vec2(world_pos, Vec2::ZERO);
    let sim_z = f32::from(story_level) * 18.0 + 6.0;
    let iso_screen = world_to_iso_screen(safe_pos, sim_z);
    let iso_z = iso_depth_key(safe_pos, sim_z, 6.0);
    let (base_box, assembly) = build_debris_iso_assembly(story_level);

    // Deterministic radial scatter velocity from hash of coordinates
    let angle = (safe_pos.x * 0.13 + safe_pos.y * 0.29 + f32::from(story_level) * 1.7).sin()
        * std::f32::consts::PI;
    let horiz_vel = Vec2::new(angle.cos() * 24.0, angle.sin() * 24.0);

    let debris_entity = commands
        .spawn((
            Name::new("Falling Structural Debris"),
            SiteScoped { epoch },
            DebrisTier::ActivePhysics,
            DebrisChunk {
                tier: DebrisTier::ActivePhysics,
                active_timer_secs: 4.0,
                simulated_z_height: sim_z,
                vertical_z_velocity: -65.0,
                horizontal_velocity: horiz_vel,
                spin_rate: 2.2,
            },
            WorldPose::new(safe_pos, sim_z, angle),
            IsoLayerBias(6.0),
            base_box,
            assembly,
            Transform::from_xyz(iso_screen.x, iso_screen.y, iso_z),
            GlobalTransform::default(),
            RigidBody::Dynamic,
            Collider::rectangle(12.0, 8.0),
            LinearVelocity::default(),
            AngularVelocity::default(),
        ))
        .id();

    debris_pool.register_or_recycle_oldest(debris_entity, commands);
    debris_entity
}

/// Polls in-flight async BFS structural collapse tasks, validates `SessionEpoch` and
/// `dirty_generation`, converts orphaned upper stories into falling debris clusters, and
/// rebuilds the building's 2.5D `IsoBoxAssembly`.
pub fn poll_structural_collapse_tasks_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    mut graph_registry: ResMut<BuildingGraphRegistry>,
    mut debris_pool: ResMut<DebrisPoolRegistry>,
    mut cash_events: EventWriter<DemolitionCashEvent>,
    mut particle_events: EventWriter<ParticleBurstEvent>,
    mut audio_events: EventWriter<AudioTriggerEvent>,
    mut collapse_zones: ResMut<ActiveCollapseZones>,
    mut building_ids: Local<Vec<Entity>>,
    mut building_query: Query<(
        Option<&WorldPose>,
        &Transform,
        &ModularBuilding,
        Option<&mut IsoExtrudedBox>,
        Option<&mut IsoBoxAssembly>,
    )>,
) {
    let current_epoch = *epoch;
    graph_registry.current_epoch = current_epoch;

    let task_pool = AsyncComputeTaskPool::get();
    building_ids.clear();
    building_ids.extend(graph_registry.buildings.keys().copied());

    for &building_id in building_ids.iter() {
        let mut completed_result: Option<CollapseEvalResult> = None;
        let mut needs_followup = false;

        if let Some(entry) = graph_registry.buildings.get_mut(&building_id)
            && let Some(task) = &mut entry.in_flight_task
            && let Some(res) = block_on(future::poll_once(task))
        {
            completed_result = Some(res);
            entry.in_flight_task = None;
            needs_followup = entry.pending_recheck;
        }

        if let Some(res) = completed_result {
            // Epoch Guard: Discard results from an older SessionEpoch!
            if res.epoch != current_epoch {
                continue;
            }
            // Verify entity still exists in ECS World.
            if commands.get_entity(building_id).is_none() {
                continue;
            }

            let base_pos = building_query
                .get(building_id)
                .map(|(pose, tf, _, _, _)| pose.map_or_else(|| tf.translation.truncate(), |p| p.pos))
                .unwrap_or(Vec2::ZERO);

            if let Some(entry) = graph_registry.buildings.get_mut(&building_id)
                && (entry.dirty_generation == res.generation || !needs_followup)
            {
                for &stressed_id in &res.stressed_neighbors {
                    if let Some(node) = entry.nodes.get_mut(&stressed_id)
                        && node.intact
                        && node.damage_state == DamageState::Intact
                    {
                        node.damage_state = DamageState::CrackedStressed;
                    }
                }

                if !res.orphaned_nodes.is_empty() {
                    collapse_zones.zones.push(CollapseZone {
                        center: base_pos,
                        radius: 140.0,
                        epoch: current_epoch,
                    });
                    particle_events.send(ParticleBurstEvent {
                        kind: ParticleBurstKind::MasonryDust,
                        world_pos: base_pos,
                        count: 10,
                    });
                    audio_events.send(AudioTriggerEvent {
                        kind: AudioSfxKind::CollapseRumble,
                    });
                }

                for &orphan_id in &res.orphaned_nodes {
                    if let Some(node) = entry.nodes.get_mut(&orphan_id)
                        && node.intact
                    {
                        node.intact = false;
                        node.damage_state = DamageState::RubbleDebris;
                        let local_xy =
                            Vec2::new(node.grid_x as f32 * 21.0, node.grid_y as f32 * 15.0);
                        let world_xy = base_pos + local_xy;
                        let screen_xy =
                            world_to_iso_screen(world_xy, f32::from(node.story_level) * 16.0);

                        cash_events.send(DemolitionCashEvent {
                            amount: node.cash_value,
                            world_pos: screen_xy.extend(0.0),
                        });

                        spawn_pooled_debris_chunk(
                            &mut commands,
                            &mut debris_pool,
                            current_epoch,
                            world_xy,
                            node.story_level,
                        );
                    }
                }

                // Rebuild 2.5D building assembly to reflect newly destroyed/stressed nodes
                if let Ok((_, _, b_meta, Some(mut base_box), Some(mut assembly))) =
                    building_query.get_mut(building_id)
                {
                    let prev_alpha = assembly.alpha_multiplier;
                    let (mut new_base, mut new_asm) = build_building_iso_assembly(
                        &entry.nodes,
                        b_meta.stories,
                        b_meta.palette_index,
                    );
                    new_base.alpha = prev_alpha;
                    new_asm.alpha_multiplier = prev_alpha;
                    *base_box = new_base;
                    *assembly = new_asm;
                }
            }

            if needs_followup {
                let _ = queue_structural_integrity_evaluation(
                    building_id,
                    &[],
                    &mut graph_registry,
                    task_pool,
                );
            }
        }
    }
}

/// Updates the 3-Tier Debris Lifecycle in Cartesian `WorldPose` space:
/// - Tier 1 (`ActivePhysics`) -> Tier 2 (`SleepingInstanced`) once velocity settles or timer expires
/// - Updates `DebugTelemetry` counts for the HUD Debug overlay
pub fn update_debris_lifecycle_system(
    mut commands: Commands,
    time: Res<Time>,
    mut road_graph: ResMut<RoadSplineGraph>,
    mut debris_pool: ResMut<DebrisPoolRegistry>,
    graph_registry: Res<BuildingGraphRegistry>,
    mut telemetry: ResMut<DebugTelemetry>,
    mut debris_query: Query<(
        Entity,
        &mut DebrisChunk,
        Option<&mut WorldPose>,
        &mut Transform,
        Option<&LinearVelocity>,
    )>,
) {
    let Ok(dt) = sanitize_dt(time.delta_secs()) else {
        return;
    };

    let mut active_count: usize = 0;
    let mut sleeping_count: usize = 0;

    for (entity, mut chunk, mut world_pose, mut transform, lin_vel) in &mut debris_query {
        match chunk.tier {
            DebrisTier::ActivePhysics => {
                active_count = active_count.saturating_add(1);
                chunk.active_timer_secs -= dt;
                if chunk.simulated_z_height > 0.0 {
                    chunk.vertical_z_velocity =
                        sanitize_f32(chunk.vertical_z_velocity - 180.0 * dt, -65.0);
                    let dz = chunk.vertical_z_velocity * dt;
                    chunk.simulated_z_height = (chunk.simulated_z_height + dz).max(0.0);

                    if let Some(pose) = world_pose.as_deref_mut() {
                        pose.pos = sanitize_vec2(
                            pose.pos + chunk.horizontal_velocity * dt,
                            pose.pos,
                        );
                        pose.elev_z = chunk.simulated_z_height;
                        pose.heading_rad = sanitize_f32(
                            pose.heading_rad + chunk.spin_rate * dt,
                            pose.heading_rad,
                        );
                        pose.pitch_rad = (chunk.simulated_z_height * 0.08).sin() * 0.35;
                    } else {
                        transform.translation.y =
                            sanitize_f32(transform.translation.y + dz, transform.translation.y);
                    }
                } else if let Some(pose) = world_pose.as_deref_mut() {
                    pose.elev_z = 0.0;
                    pose.pitch_rad = 0.0;
                    chunk.horizontal_velocity *= (1.0 - dt * 6.0).clamp(0.0, 1.0);
                }

                let speed_sq = lin_vel.map_or_else(
                    || chunk.horizontal_velocity.length_squared(),
                    |v| sanitize_vec2(v.0, Vec2::ZERO).length_squared(),
                );
                let settled = chunk.active_timer_secs <= 0.0
                    || (chunk.simulated_z_height <= 0.01 && speed_sq < 4.0);

                if settled {
                    chunk.tier = DebrisTier::SleepingInstanced;
                    let world_xy = world_pose
                        .as_deref()
                        .map_or_else(|| transform.translation.truncate(), |p| p.pos);
                    road_graph.mark_rubble_obstruction(world_xy, 28.0);
                    debris_pool.active_ring.retain(|&e| e != entity);

                    if let Some(mut entity_cmds) = commands.get_entity(entity) {
                        entity_cmds
                            .remove::<RigidBody>()
                            .remove::<Collider>()
                            .remove::<LinearVelocity>()
                            .remove::<AngularVelocity>()
                            .insert(DebrisTier::SleepingInstanced);
                    }
                }
            }
            DebrisTier::SleepingInstanced => {
                sleeping_count = sleeping_count.saturating_add(1);
            }
            DebrisTier::CrushedDecal => {}
        }
    }

    let mut intact_nodes: usize = 0;
    let mut in_flight: usize = 0;
    for entry in graph_registry.buildings.values() {
        if entry.in_flight_task.is_some() {
            in_flight = in_flight.saturating_add(1);
        }
        intact_nodes = intact_nodes
            .saturating_add(entry.nodes.values().filter(|n| n.intact).count());
    }

    telemetry.active_physics_debris = active_count;
    telemetry.sleeping_rubble_count = sleeping_count;
    telemetry.active_bfs_tasks = in_flight;
    telemetry.intact_structural_nodes = intact_nodes;
}

/// Spawns initial modular buildings, interior flooring, interior furniture, and exterior street
/// props in Cartesian `WorldPose` space with 2.5D multi-box assemblies (`Issues #7 and #10`).
pub fn ensure_site_buildings_spawned_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    env_config: Res<EnvironmentConfig>,
    manifest: Res<CampaignPrefabsManifest>,
    _atlas: Res<ProceduralWorldAtlas>,
    mut graph_registry: ResMut<BuildingGraphRegistry>,
    existing_buildings: Query<Entity, With<ModularBuilding>>,
) {
    if existing_buildings.iter().next().is_some() {
        return;
    }

    graph_registry.current_epoch = *epoch;
    let level_idx = env_config.level.index_one_based();
    let stories = manifest
        .entry_for_level(env_config.level)
        .map_or_else(
            || match env_config.level {
                CampaignLevel::County1 => 1,
                CampaignLevel::Village2 | CampaignLevel::Township3 | CampaignLevel::Suburb4 => 2,
                CampaignLevel::CityBorough5 => 4,
                CampaignLevel::CityDowntown6 => 6,
                CampaignLevel::GovernorsMansion7 => 3,
            },
            |e| e.max_stories,
        )
        .clamp(1, 8);

    let offsets = [
        Vec2::new(-160.0, 120.0),
        Vec2::new(160.0, 120.0),
        Vec2::new(0.0, 220.0),
    ];

    let interior_props = [
        PropArchetype::CashRegister,
        PropArchetype::Toilet,
        PropArchetype::Refrigerator,
        PropArchetype::Couch,
        PropArchetype::GlassDisplayCase,
        PropArchetype::Forklift,
    ];

    for (idx, &offset) in offsets.iter().enumerate() {
        let nodes = build_procedural_building_graph(stories, 2);
        let archetype_name = manifest
            .entry_for_level(env_config.level)
            .and_then(|e| e.archetypes.get(idx))
            .cloned()
            .unwrap_or_else(|| env_config.level.label().to_string());

        // Spawn 2.5D interior floor slab (revealed when exterior/roof cutaway fades!)
        let floor_iso = world_to_iso_screen(offset, 0.5);
        let floor_z = iso_depth_key(offset, 0.5, 1.5);
        commands.spawn((
            Name::new(format!("Building #{idx} Interior Flooring")),
            SiteScoped { epoch: *epoch },
            WorldPose::new(offset, 0.5, 0.0),
            IsoLayerBias(1.5),
            IsoExtrudedBox::new(104.0, 74.0, 1.8, Color::srgb(0.82, 0.78, 0.68)).with_shadow(false),
            Transform::from_xyz(floor_iso.x, floor_iso.y, floor_z),
            GlobalTransform::default(),
            Visibility::default(),
        ));

        // Spawn 4 interior furniture pieces inside this building footprint
        for p_i in 0..4_usize {
            let prop_arch = interior_props
                .get((idx * 2 + p_i) % interior_props.len())
                .copied()
                .unwrap_or(PropArchetype::Couch);
            let local_offset = Vec2::new(
                ((p_i % 2) as f32 - 0.5) * 44.0,
                ((p_i / 2) as f32 - 0.5) * 28.0,
            );
            let prop_pos = offset + local_offset;
            let prop_iso = world_to_iso_screen(prop_pos, 2.0);
            let prop_z = iso_depth_key(prop_pos, 2.0, 4.0);
            let (prop_base, prop_asm) = build_prop_iso_assembly(prop_arch);

            commands.spawn((
                Name::new(format!("Interior Prop ({prop_arch:?})")),
                SiteScoped { epoch: *epoch },
                DestructibleProp {
                    archetype: prop_arch,
                    is_interior: true,
                },
                WorldPose::new(prop_pos, 2.0, 0.0),
                IsoLayerBias(4.0),
                prop_base,
                prop_asm,
                Transform::from_xyz(prop_iso.x, prop_iso.y, prop_z),
                GlobalTransform::default(),
                Visibility::default(),
            ));
        }

        let entry = BuildingGraphEntry::new(*epoch, nodes);
        let (b_base, b_asm) = build_building_iso_assembly(&entry.nodes, stories, idx);
        let b_iso = world_to_iso_screen(offset, 0.0);
        let b_z = iso_depth_key(offset, f32::from(stories) * 16.0, 10.0);

        let b_entity = commands
            .spawn((
                Name::new(format!("Modular Building #{idx}: {archetype_name} (L{level_idx})")),
                SiteScoped { epoch: *epoch },
                ModularBuilding {
                    archetype_name,
                    stories,
                    palette_index: idx,
                },
                CutawayFootprint {
                    min_world: offset - Vec2::splat(72.0),
                    max_world: offset + Vec2::splat(72.0),
                    exterior_breached: false,
                    current_roof_alpha: 1.0,
                },
                WorldPose::new(offset, 0.0, 0.0),
                IsoLayerBias(10.0),
                b_base,
                b_asm,
                Transform::from_xyz(b_iso.x, b_iso.y, b_z),
                GlobalTransform::default(),
                Visibility::default(),
                RigidBody::Static,
                Collider::rectangle(108.0, 76.0),
            ))
            .id();

        graph_registry.buildings.insert(b_entity, entry);
    }

    // Spawn exterior street props (PropaneTanks, FireHydrants, Dumpsters, Mailboxes)
    let exterior_specs = [
        (PropArchetype::PropaneTank, Vec2::new(-230.0, 90.0)),
        (PropArchetype::FireHydrant, Vec2::new(-90.0, 40.0)),
        (PropArchetype::Dumpster, Vec2::new(230.0, 95.0)),
        (PropArchetype::Mailbox, Vec2::new(90.0, 40.0)),
        (PropArchetype::PropaneTank, Vec2::new(75.0, 190.0)),
        (PropArchetype::ConcreteBarrier, Vec2::new(-75.0, 190.0)),
    ];

    for (arch, pos) in exterior_specs {
        let prop_iso = world_to_iso_screen(pos, 0.0);
        let prop_z = iso_depth_key(pos, 6.0, 6.0);
        let (prop_base, prop_asm) = build_prop_iso_assembly(arch);
        commands.spawn((
            Name::new(format!("Exterior Street Prop ({arch:?})")),
            SiteScoped { epoch: *epoch },
            DestructibleProp {
                archetype: arch,
                is_interior: false,
            },
            WorldPose::new(pos, 0.0, 0.0),
            IsoLayerBias(6.0),
            prop_base,
            prop_asm,
            Transform::from_xyz(prop_iso.x, prop_iso.y, prop_z),
            GlobalTransform::default(),
            Visibility::default(),
        ));
    }
}

/// Resets `BuildingGraphRegistry` and `DebrisPoolRegistry` on `SiteRestartRequested`.
pub fn reset_destruction_on_site_restart_system(
    mut restart_events: EventReader<SiteRestartRequested>,
    epoch: Res<SessionEpoch>,
    mut graph_registry: ResMut<BuildingGraphRegistry>,
    mut debris_pool: ResMut<DebrisPoolRegistry>,
) {
    let mut restarted = false;
    for _ in restart_events.read() {
        restarted = true;
    }
    if !restarted {
        return;
    }

    graph_registry.clear_for_epoch(*epoch);
    debris_pool.clear();
}

/// Bevy plugin for `pd_destruction`.
pub struct PdDestructionPlugin;

impl Plugin for PdDestructionPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(CampaignPrefabsManifest::load_embedded())
            .init_resource::<BuildingGraphRegistry>()
            .init_resource::<DebrisPoolRegistry>()
            .add_systems(
                Update,
                (
                    reset_destruction_on_site_restart_system,
                    ensure_site_buildings_spawned_system.run_if(in_state(AppState::Playing)),
                )
                    .chain()
                    .after(PdSystemSet::InputAndClock)
                    .before(PdSystemSet::WorldTilemapSync),
            )
            .add_systems(
                FixedUpdate,
                (
                    poll_structural_collapse_tasks_system.in_set(PdSystemSet::DestructionGraph),
                    update_debris_lifecycle_system.in_set(PdSystemSet::DebrisLifecycle),
                )
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::tasks::TaskPoolBuilder;

    #[test]
    fn test_ron_manifest_loads_all_7_campaign_levels() {
        let manifest = CampaignPrefabsManifest::load_embedded();
        assert_eq!(manifest.levels.len(), 7);
        for level in CampaignLevel::ALL {
            assert!(manifest.entry_for_level(level).is_some());
        }
    }

    #[test]
    fn test_disconnected_upper_story_and_single_node_extremes() {
        let task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPoolBuilder::new().build());
        let mut registry = BuildingGraphRegistry::default();
        let fake_entity = Entity::from_raw(42);

        // 1. Disconnected single-node upper story floating at story_level = 2
        let floating_node = StructuralNode {
            id: 0,
            kind: StructuralModuleKind::RoofCorrugation,
            grid_x: 0,
            grid_y: 0,
            story_level: 2,
            is_foundation: false,
            intact: true,
            damage_state: DamageState::Intact,
            cash_value: 16,
            neighbors: vec![0], // self-cycle edge case
        };
        registry.buildings.insert(
            fake_entity,
            BuildingGraphEntry::new(SessionEpoch(0), vec![floating_node]),
        );

        let status1 =
            queue_structural_integrity_evaluation(fake_entity, &[], &mut registry, task_pool);
        assert_eq!(status1, EvalStatus::Dispatched);

        // Concurrent evaluation while task is in-flight coalesces cleanly
        let status2 =
            queue_structural_integrity_evaluation(fake_entity, &[], &mut registry, task_pool);
        assert_eq!(status2, EvalStatus::Coalesced);

        // Wait for task completion and verify orphan detection
        if let Some(entry) = registry.buildings.get_mut(&fake_entity)
            && let Some(task) = entry.in_flight_task.take()
        {
            let result = block_on(task);
            assert_eq!(result.orphaned_nodes, vec![0]);
        }
    }

    #[test]
    fn test_pillar_removal_disconnects_upper_floors() {
        let nodes = build_procedural_building_graph(2, 1);
        let foundation_ids: Vec<NodeIndex> = nodes
            .iter()
            .filter(|n| n.story_level == 0)
            .map(|n| n.id)
            .collect();

        let mut map: HashMap<NodeIndex, StructuralNode> =
            nodes.into_iter().map(|n| (n.id, n)).collect();
        for &fid in &foundation_ids {
            if let Some(n) = map.get_mut(&fid) {
                n.intact = false;
            }
        }

        let res = evaluate_orphaned_nodes_bounded(
            Entity::from_raw(7),
            SessionEpoch(1),
            1,
            &map,
            &foundation_ids,
        );
        // All 9 upper-story nodes (3x3 at story_level == 1) must be identified as orphaned
        assert_eq!(res.orphaned_nodes.len(), 9);
    }
}
