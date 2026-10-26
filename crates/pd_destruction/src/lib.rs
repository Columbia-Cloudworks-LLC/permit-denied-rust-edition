//! `pd_destruction`: Spatial per-cell modular building graphs with material HP & cladding (#14),
//! merged exterior facade spans (`WallSpan`) & locally supported sagging roof bays (`Gable`,
//! `Shed`, `Flat`) with domino lean (#15), 4-layer mass-conserving debris lifecycle with
//! `PileField` height/resistance transfer & `CollapsedSite` scars (#16), and a data-driven
//! destructible prop catalog with 7 destruction profiles (`Brittle`, `BendSnap`, `Crush`,
//! `Topple`, `Roll`, `PanelCollapse`, `Explosive`) (#17).
//!
//! Depends only on `pd_core` and `pd_world`.

use avian2d::prelude::*;
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
pub use pd_core::MaterialKind;
use pd_core::{
    ActiveCollapseZones, AppState, AudioSfxKind, AudioTriggerEvent, CampaignLevel, CollapseZone,
    DebugTelemetry, DecalKind, DecalSpawnRequest, DemolitionCashEvent, EnvironmentConfig,
    ISO_BIAS_BUILDING, ISO_BIAS_DEBRIS, ISO_BIAS_INTERIOR_FLOOR, ISO_BIAS_PROP, IsoBoxAssembly,
    IsoExtrudedBox, IsoLayerBias, ParticleBurstEvent, ParticleBurstKind, PdSystemSet,
    RANCH_SITE_ORIGIN, RoofSlopeSpec, SIM_ORDINAL_DEBRIS_BASE, SIM_ORDINAL_PROP_BASE, SessionEpoch, SimOrdinal,
    SimTick, SiteRestartRequested, SiteTestMap, SiteScoped, WorldPose, iso_depth_key,
    sanitize_dt, sanitize_f32, sanitize_vec2, world_to_iso_screen,
};
use pd_world::{
    CollapsedSite, CutawayFootprint, PileField, ProceduralWorldAtlas, RoadSplineGraph,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Embedded `.ron` campaign level prefab definitions guaranteeing zero-panic loading.
pub const EMBEDDED_CAMPAIGN_PREFABS_RON: &str =
    include_str!("../../../assets/prefabs/campaign_levels.prefab.ron");

/// Hard ceiling on active dynamic physics debris bodies per the PRD performance guardrail.
pub const MAX_ACTIVE_PHYSICS_DEBRIS: usize = 500;

/// Soft cleanup threshold where distant unattended rubble is absorbed into `PileField`.
pub const SOFT_DEBRIS_CLEANUP_THRESHOLD: usize = 360;

/// Hard iteration cap on Breadth-First Search (BFS) structural connectivity evaluation.
pub const MAX_GRAPH_ITERATIONS: usize = 4_096;

/// Maximum recursion generation for chained `Explosive` prop blasts (`0 -> 1`).
pub const MAX_EXPLOSION_GENERATION: u8 = 1;

/// Cartesian X spacing of a single building grid cell (world units).
pub const BUILDING_CELL_STEP_X: f32 = 21.0;

/// Cartesian Y spacing of a single building grid cell (world units).
pub const BUILDING_CELL_STEP_Y: f32 = 15.0;

/// Cartesian X width of a single building cell collider/box (`5 * 21.0 + 3.0 = 108.0` for `half = 2`).
pub const BUILDING_CELL_WIDTH_X: f32 = 24.0;

/// Cartesian Y depth of a single building cell collider/box (`5 * 15.0 + 1.0 = 76.0` for `half = 2`).
pub const BUILDING_CELL_DEPTH_Y: f32 = 16.0;

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

/// 7 data-driven destruction behavior profiles (`catalog.ts` & `objectBehavior.ts` / Issue #17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DestructionProfile {
    /// Shatters immediately into small fragments and `PileField` mass.
    Brittle,
    /// Bends progressively away from the blade (`lean_progress: 0.0 -> 1.0`) before snapping.
    BendSnap,
    /// Compacts vertically under blade/track pressure and transfers mass into `PileField`.
    Crush,
    /// Tilts and topples along the impact direction into a pushable elongated remnant.
    Topple,
    /// Detaches and rolls away as a high-spin cylindrical physics remnant.
    Roll,
    /// Collapses into flat pushable panel remnants.
    PanelCollapse,
    /// Detonates on destruction, applying blast damage to nearby building cells and props up to `max_generation`.
    Explosive { max_generation: u8 },
}

/// Data-driven catalog definition for a destructible prop archetype (`catalog.ts` / Issue #17).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AssetCatalogDef {
    pub archetype: PropArchetype,
    pub profile: DestructionProfile,
    pub material: MaterialKind,
    pub max_hp: f32,
    pub mass: f32,
    pub blade_damage_mul: f32,
    pub track_hazard_stress: f32,
    pub cash_value: u32,
    pub blast_radius: f32,
    pub blast_damage: f32,
}

/// Interior and exterior prop archetypes per Section 4 of the PRD and Issue #17.
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
    // Exterior & site props covering all 7 destruction profiles
    Mailbox,
    StreetSign,
    UtilityPole,
    FireHydrant,
    Dumpster,
    PropaneTank,
    WoodenPallet,
    ConcreteBarrier,
    PineTree,
    HayBale,
    FuelDrum,
}

impl PropArchetype {
    #[must_use]
    pub const fn catalog_def(self) -> AssetCatalogDef {
        match self {
            Self::CashRegister => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Brittle,
                material: MaterialKind::Steel,
                max_hp: 14.0,
                mass: 0.9,
                blade_damage_mul: 1.35,
                track_hazard_stress: 0.0,
                cash_value: 89,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::Forklift => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Topple,
                material: MaterialKind::Steel,
                max_hp: 38.0,
                mass: 3.4,
                blade_damage_mul: 1.0,
                track_hazard_stress: 0.14,
                cash_value: 71,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::PropaneTank => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Explosive {
                    max_generation: MAX_EXPLOSION_GENERATION,
                },
                material: MaterialKind::Steel,
                max_hp: 18.0,
                mass: 1.6,
                blade_damage_mul: 1.4,
                track_hazard_stress: 0.08,
                cash_value: 65,
                blast_radius: 92.0,
                blast_damage: 85.0,
            },
            Self::GlassDisplayCase => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Brittle,
                material: MaterialKind::Glass,
                max_hp: 10.0,
                mass: 0.8,
                blade_damage_mul: 1.6,
                track_hazard_stress: 0.0,
                cash_value: 45,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::Refrigerator => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Topple,
                material: MaterialKind::Steel,
                max_hp: 24.0,
                mass: 1.9,
                blade_damage_mul: 1.15,
                track_hazard_stress: 0.05,
                cash_value: 45,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::Television => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Brittle,
                material: MaterialKind::Glass,
                max_hp: 8.0,
                mass: 0.6,
                blade_damage_mul: 1.6,
                track_hazard_stress: 0.0,
                cash_value: 32,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::Dumpster => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Crush,
                material: MaterialKind::Steel,
                max_hp: 34.0,
                mass: 2.8,
                blade_damage_mul: 1.1,
                track_hazard_stress: 0.10,
                cash_value: 32,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::FireHydrant => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::BendSnap,
                material: MaterialKind::Steel,
                max_hp: 20.0,
                mass: 1.2,
                blade_damage_mul: 1.25,
                track_hazard_stress: 0.12,
                cash_value: 32,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::Couch | Self::Bed => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Crush,
                material: MaterialKind::Wood,
                max_hp: 14.0,
                mass: 1.2,
                blade_damage_mul: 1.4,
                track_hazard_stress: 0.0,
                cash_value: 16,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::DiningTable => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::PanelCollapse,
                material: MaterialKind::Wood,
                max_hp: 15.0,
                mass: 1.1,
                blade_damage_mul: 1.35,
                track_hazard_stress: 0.0,
                cash_value: 16,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::Toilet => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Brittle,
                material: MaterialKind::Concrete,
                max_hp: 12.0,
                mass: 0.9,
                blade_damage_mul: 1.5,
                track_hazard_stress: 0.0,
                cash_value: 16,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::MetalShelving => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::PanelCollapse,
                material: MaterialKind::Steel,
                max_hp: 20.0,
                mass: 1.5,
                blade_damage_mul: 1.25,
                track_hazard_stress: 0.04,
                cash_value: 12,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::Bookshelf | Self::IndustrialCrate | Self::OfficeDesk | Self::FilingCabinet => {
                AssetCatalogDef {
                    archetype: self,
                    profile: DestructionProfile::Crush,
                    material: MaterialKind::Wood,
                    max_hp: 16.0,
                    mass: 1.3,
                    blade_damage_mul: 1.3,
                    track_hazard_stress: 0.0,
                    cash_value: 12,
                    blast_radius: 0.0,
                    blast_damage: 0.0,
                }
            }
            Self::ConcreteBarrier => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Crush,
                material: MaterialKind::Concrete,
                max_hp: 42.0,
                mass: 3.2,
                blade_damage_mul: 1.15,
                track_hazard_stress: 0.16,
                cash_value: 12,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::StreetSign => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::BendSnap,
                material: MaterialKind::Steel,
                max_hp: 14.0,
                mass: 0.7,
                blade_damage_mul: 1.4,
                track_hazard_stress: 0.06,
                cash_value: 5,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::UtilityPole => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::BendSnap,
                material: MaterialKind::Wood,
                max_hp: 28.0,
                mass: 2.2,
                blade_damage_mul: 1.2,
                track_hazard_stress: 0.22,
                cash_value: 18,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::PineTree => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Topple,
                material: MaterialKind::Wood,
                max_hp: 26.0,
                mass: 2.4,
                blade_damage_mul: 1.25,
                track_hazard_stress: 0.08,
                cash_value: 15,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::HayBale | Self::FuelDrum => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Roll,
                material: if matches!(self, Self::HayBale) {
                    MaterialKind::Wood
                } else {
                    MaterialKind::Steel
                },
                max_hp: 14.0,
                mass: 1.4,
                blade_damage_mul: 1.35,
                track_hazard_stress: 0.0,
                cash_value: 10,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
            Self::Mailbox | Self::WoodenPallet => AssetCatalogDef {
                archetype: self,
                profile: DestructionProfile::Brittle,
                material: MaterialKind::Wood,
                max_hp: 10.0,
                mass: 0.6,
                blade_damage_mul: 1.5,
                track_hazard_stress: 0.0,
                cash_value: 5,
                blast_radius: 0.0,
                blast_damage: 0.0,
            },
        }
    }

    #[must_use]
    pub const fn cash_value(self) -> u32 {
        self.catalog_def().cash_value
    }

    #[must_use]
    pub const fn color(self) -> Color {
        match self {
            Self::CashRegister => Color::srgb(0.96, 0.82, 0.24),
            Self::PropaneTank | Self::FireHydrant | Self::FuelDrum => {
                Color::srgb(0.92, 0.24, 0.18)
            }
            Self::Toilet | Self::Refrigerator | Self::GlassDisplayCase => {
                Color::srgb(0.86, 0.92, 0.96)
            }
            Self::Forklift | Self::HayBale => Color::srgb(0.92, 0.64, 0.14),
            Self::Dumpster | Self::PineTree => Color::srgb(0.22, 0.48, 0.28),
            Self::UtilityPole | Self::StreetSign => Color::srgb(0.48, 0.42, 0.34),
            _ => Color::srgb(0.64, 0.46, 0.28),
        }
    }
}

/// Component attached to destructible interior furniture and exterior street props.
#[derive(Component, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DestructibleProp {
    pub archetype: PropArchetype,
    pub is_interior: bool,
    pub hp: f32,
    pub max_hp: f32,
    pub lean_progress: f32,
    pub lean_dir: Vec2,
    pub crush_progress: f32,
    pub chain_generation: u8,
}

impl Default for DestructibleProp {
    fn default() -> Self {
        Self::new(PropArchetype::WoodenPallet, false)
    }
}

impl DestructibleProp {
    #[must_use]
    pub const fn new(archetype: PropArchetype, is_interior: bool) -> Self {
        let def = archetype.catalog_def();
        Self {
            archetype,
            is_interior,
            hp: def.max_hp,
            max_hp: def.max_hp,
            lean_progress: 0.0,
            lean_dir: Vec2::X,
            crush_progress: 0.0,
            chain_generation: 0,
        }
    }

    #[must_use]
    pub const fn with_chain_generation(mut self, generation: u8) -> Self {
        self.chain_generation = generation;
        self
    }
}

/// Single node in a building's spatial per-cell structural graph (`Issue #14`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralNode {
    pub id: NodeIndex,
    pub kind: StructuralModuleKind,
    pub material: MaterialKind,
    pub facade_material: MaterialKind,
    pub hp: f32,
    pub max_hp: f32,
    pub cladding_hp: f32,
    pub cell_min: Vec2,
    pub cell_max: Vec2,
    pub grid_x: i32,
    pub grid_y: i32,
    pub story_level: u8,
    pub is_foundation: bool,
    pub intact: bool,
    pub damage_state: DamageState,
    pub cash_value: u32,
    pub neighbors: Vec<NodeIndex>,
}

impl StructuralNode {
    /// Computes default local cell bounds `(cell_min, cell_max)` for grid coordinates `(gx, gy)`.
    #[must_use]
    pub fn cell_bounds_for_grid(gx: i32, gy: i32) -> (Vec2, Vec2) {
        let center = Vec2::new(
            (gx as f32) * BUILDING_CELL_STEP_X,
            (gy as f32) * BUILDING_CELL_STEP_Y,
        );
        let half = Vec2::new(BUILDING_CELL_WIDTH_X * 0.5, BUILDING_CELL_DEPTH_Y * 0.5);
        (center - half, center + half)
    }

    /// Returns world-space `(min, max)` AABB bounds for this cell given `building_origin`.
    #[must_use]
    pub fn world_bounds(&self, building_origin: Vec2) -> (Vec2, Vec2) {
        let safe_origin = sanitize_vec2(building_origin, Vec2::ZERO);
        (safe_origin + self.cell_min, safe_origin + self.cell_max)
    }

    /// Returns world-space center of this cell given `building_origin`.
    #[must_use]
    pub fn world_center(&self, building_origin: Vec2) -> Vec2 {
        let (w_min, w_max) = self.world_bounds(building_origin);
        (w_min + w_max) * 0.5
    }

    /// Returns `true` if this node is an intact ground-floor solid obstacle (`Wall` or `LoadBearingPillar`).
    #[must_use]
    pub const fn is_solid_ground_obstacle(&self) -> bool {
        self.intact
            && self.story_level == 0
            && matches!(
                self.kind,
                StructuralModuleKind::Wall | StructuralModuleKind::LoadBearingPillar
            )
    }

    /// Applies localized structural damage to cladding first, then core `hp`.
    /// Transitions to `CrackedStressed` when `hp <= 0.58 * max_hp` and returns `true` when breached (`hp <= 0.0`).
    pub fn apply_localized_damage(&mut self, raw_damage: f32) -> bool {
        let dmg = sanitize_f32(raw_damage, 0.0).max(0.0);
        if !self.intact || dmg <= 0.0 {
            return false;
        }

        let mut remaining = dmg;
        if self.cladding_hp > 0.0 {
            let soaked = remaining.min(self.cladding_hp);
            self.cladding_hp = sanitize_f32(self.cladding_hp - soaked, 0.0).max(0.0);
            remaining = (remaining - soaked).max(0.0);
        }

        self.hp = sanitize_f32(self.hp - remaining, 0.0).max(0.0);
        if self.hp <= 0.0 {
            self.intact = false;
            self.damage_state = DamageState::RubbleDebris;
            true
        } else {
            let max_hp = self.max_hp.max(1.0);
            if self.hp <= max_hp * 0.58
                || (self.kind == StructuralModuleKind::LoadBearingPillar && self.cladding_hp < 6.0)
            {
                self.damage_state = DamageState::CrackedStressed;
            }
            false
        }
    }
}

/// Roof architectural style (`roof.ts` / Issue #15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum RoofStyle {
    #[default]
    Gable,
    Shed,
    Flat,
}

/// Lifecycle state of an individual locally supported roof bay (`roof.ts` / Issue #15).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum RoofBayState {
    Intact,
    Sagging { progress: f32, hinge_dir: Vec2 },
    Collapsed,
}

/// Locally supported roof bay spanning a contiguous section of a building's roof (`Issue #15`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoofBay {
    pub bay_id: u32,
    pub style: RoofStyle,
    pub state: RoofBayState,
    pub cell_min: Vec2,
    pub cell_max: Vec2,
    pub z_base: f32,
    pub height: f32,
    pub material: MaterialKind,
    pub support_nodes: Vec<NodeIndex>,
    pub roof_nodes: Vec<NodeIndex>,
    pub mass: f32,
}

/// Orientation of a merged exterior wall span (`buildingSurfaces.ts` / Issue #15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WallSpanOrientation {
    HorizontalX,
    VerticalY,
}

/// Contiguous run of intact or cracked exterior wall cells sharing `(story_level, material, damage_state)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallSpan {
    pub orientation: WallSpanOrientation,
    pub story_level: u8,
    pub material: MaterialKind,
    pub damage_state: DamageState,
    pub local_center: Vec2,
    pub length: f32,
    pub width: f32,
    pub z0: f32,
    pub height: f32,
    pub cell_count: usize,
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

/// Per-building structural graph, merged facade span cache, and roof bay state managed in `BuildingGraphRegistry`.
#[derive(Debug)]
pub struct BuildingGraphEntry {
    pub epoch: SessionEpoch,
    pub dirty_generation: u64,
    pub visual_revision: u64,
    pub cached_spans_revision: u64,
    pub cached_wall_spans: Vec<WallSpan>,
    pub roof_style: RoofStyle,
    pub roof_bays: Vec<RoofBay>,
    pub initial_node_count: usize,
    pub collapse_bonus_paid: bool,
    pub collapsed_site_spawned: bool,
    pub full_collapse_bonus: u32,
    pub collapsing: bool,
    pub pending_recheck: bool,
    pub nodes: HashMap<NodeIndex, StructuralNode>,
    pub in_flight_task: Option<Task<CollapseEvalResult>>,
    /// [`SimTick`] on which the in-flight BFS result is applied (dispatch tick +
    /// [`COLLAPSE_EVAL_LATENCY_TICKS`]), never earlier, so collapse timing is deterministic.
    pub in_flight_due_tick: u64,
    /// Stable spawn index on the site. Order-sensitive passes over buildings sort by this,
    /// never by `Entity` (indices are recycled) or `HashMap` order (randomly seeded).
    pub site_ordinal: u32,
}

impl BuildingGraphEntry {
    #[must_use]
    pub fn new(epoch: SessionEpoch, nodes_list: Vec<StructuralNode>) -> Self {
        let initial_node_count = nodes_list.len().max(1);
        let max_story = nodes_list
            .iter()
            .map(|n| n.story_level)
            .max()
            .unwrap_or(0);
        let stories = max_story.saturating_add(1);
        let roof_style = match stories {
            1 => RoofStyle::Gable,
            2 => RoofStyle::Shed,
            _ => RoofStyle::Flat,
        };
        let mut nodes = HashMap::with_capacity(nodes_list.len());
        for node in nodes_list {
            nodes.insert(node.id, node);
        }
        let roof_bays = build_roof_bays_for_nodes(&nodes, roof_style);
        let cached_wall_spans = extract_merged_wall_spans(&nodes);
        Self {
            epoch,
            dirty_generation: 0,
            visual_revision: 1,
            cached_spans_revision: 1,
            cached_wall_spans,
            roof_style,
            roof_bays,
            initial_node_count,
            collapse_bonus_paid: false,
            collapsed_site_spawned: false,
            full_collapse_bonus: u32::from(stories).saturating_mul(120),
            collapsing: false,
            pending_recheck: false,
            nodes,
            in_flight_task: None,
            in_flight_due_tick: 0,
            site_ordinal: 0,
        }
    }

    /// Sets the stable site spawn index (see [`BuildingGraphEntry::site_ordinal`]).
    #[must_use]
    pub const fn with_site_ordinal(mut self, ordinal: u32) -> Self {
        self.site_ordinal = ordinal;
        self
    }

    /// Bumps `visual_revision` and refreshes `cached_wall_spans` if stale.
    pub fn refresh_surface_cache(&mut self) {
        if self.cached_spans_revision != self.visual_revision {
            self.cached_wall_spans = extract_merged_wall_spans(&self.nodes);
            self.cached_spans_revision = self.visual_revision;
        }
    }

    /// Fraction (`0.0..=1.0`) of total building nodes that have been destroyed.
    #[must_use]
    pub fn destroyed_fraction(&self) -> f32 {
        if self.initial_node_count == 0 {
            return 0.0;
        }
        let destroyed = self.nodes.values().filter(|n| !n.intact).count();
        (destroyed as f32 / self.initial_node_count as f32).clamp(0.0, 1.0)
    }
}

/// Constructs locally supported roof bays (`West`, `Center`, `East`) for a building's node graph (`Issue #15`).
#[must_use]
pub fn build_roof_bays_for_nodes(
    nodes: &HashMap<NodeIndex, StructuralNode>,
    roof_style: RoofStyle,
) -> Vec<RoofBay> {
    if nodes.is_empty() {
        return Vec::new();
    }
    let max_story = nodes.values().map(|n| n.story_level).max().unwrap_or(0);
    let min_x = nodes.values().map(|n| n.grid_x).min().unwrap_or(-2);
    let max_x = nodes.values().map(|n| n.grid_x).max().unwrap_or(2);
    let min_y = nodes.values().map(|n| n.grid_y).min().unwrap_or(-2);
    let max_y = nodes.values().map(|n| n.grid_y).max().unwrap_or(2);

    // Partition X into 3 bays (West: gx < 0, Center: gx == 0, East: gx > 0) when span >= 3,
    // or a single bay for 1-cell buildings.
    let ranges: Vec<(u32, i32, i32)> = if max_x - min_x >= 2 {
        vec![(0, min_x, -1), (1, 0, 0), (2, 1, max_x)]
    } else {
        vec![(0, min_x, max_x)]
    };

    let story_h = 16.0_f32;
    let z_base = 3.0 + f32::from(max_story.saturating_add(1)) * story_h - 4.5;
    let mut bays = Vec::with_capacity(ranges.len());

    for (bay_id, x0, x1) in ranges {
        let mut support_nodes = Vec::new();
        let mut roof_nodes = Vec::new();
        let mut min_bound = Vec2::splat(f32::MAX);
        let mut max_bound = Vec2::splat(f32::MIN);

        for (&id, node) in nodes {
            if node.grid_x >= x0 && node.grid_x <= x1 {
                min_bound = min_bound.min(node.cell_min);
                max_bound = max_bound.max(node.cell_max);
                if matches!(node.kind, StructuralModuleKind::RoofCorrugation)
                    || (node.story_level == max_story
                        && !matches!(
                            node.kind,
                            StructuralModuleKind::Wall | StructuralModuleKind::LoadBearingPillar
                        ))
                {
                    roof_nodes.push(id);
                }
                if matches!(
                    node.kind,
                    StructuralModuleKind::Wall | StructuralModuleKind::LoadBearingPillar
                ) && (node.story_level == 0 || node.story_level == max_story)
                {
                    support_nodes.push(id);
                }
            }
        }

        support_nodes.sort_unstable();
        roof_nodes.sort_unstable();

        if !support_nodes.is_empty() || !roof_nodes.is_empty() {
            if !min_bound.is_finite() || !max_bound.is_finite() || min_bound.x > max_bound.x {
                let (c0, _) = StructuralNode::cell_bounds_for_grid(x0, min_y);
                let (_, c1) = StructuralNode::cell_bounds_for_grid(x1, max_y);
                min_bound = c0;
                max_bound = c1;
            }
            let mat = match roof_style {
                RoofStyle::Gable => MaterialKind::Wood,
                RoofStyle::Shed | RoofStyle::Flat => MaterialKind::Steel,
            };
            bays.push(RoofBay {
                bay_id,
                style: roof_style,
                state: RoofBayState::Intact,
                cell_min: min_bound,
                cell_max: max_bound,
                z_base,
                height: if roof_style == RoofStyle::Gable {
                    8.5
                } else {
                    5.5
                },
                material: mat,
                support_nodes,
                roof_nodes,
                mass: 2.6,
            });
        }
    }

    bays
}

/// Merges contiguous intact/cracked exterior wall cells sharing `(story_level, material, damage_state)`
/// into single `WallSpan` runs (`extractBuildingSurfaces` in `buildingSurfaces.ts` / Issue #15).
#[must_use]
pub fn extract_merged_wall_spans(nodes: &HashMap<NodeIndex, StructuralNode>) -> Vec<WallSpan> {
    if nodes.is_empty() {
        return Vec::new();
    }
    let max_story = nodes.values().map(|n| n.story_level).max().unwrap_or(0);
    let min_x = nodes.values().map(|n| n.grid_x).min().unwrap_or(-2);
    let max_x = nodes.values().map(|n| n.grid_x).max().unwrap_or(2);
    let min_y = nodes.values().map(|n| n.grid_y).min().unwrap_or(-2);
    let max_y = nodes.values().map(|n| n.grid_y).max().unwrap_or(2);

    let mut grid_lookup: HashMap<(i32, i32, u8), &StructuralNode> =
        HashMap::with_capacity(nodes.len());
    for node in nodes.values() {
        if node.intact && matches!(node.kind, StructuralModuleKind::Wall) {
            grid_lookup.insert((node.grid_x, node.grid_y, node.story_level), node);
        }
    }

    let story_h = 16.0_f32;
    let mut spans = Vec::new();

    for story in 0..=max_story {
        let z0 = 3.0 + f32::from(story) * story_h;

        // 1. Horizontal wall runs along South (min_y) and North (max_y) facades
        let y_rows = if min_y == max_y {
            vec![min_y]
        } else {
            vec![min_y, max_y]
        };
        for gy in y_rows {
            let mut gx = min_x;
            while gx <= max_x {
                if let Some(&first_node) = grid_lookup.get(&(gx, gy, story)) {
                    let mat = first_node.facade_material;
                    let dmg = first_node.damage_state;
                    let start_x = gx;
                    let mut end_x = gx;
                    while end_x < max_x {
                        if let Some(&next_node) = grid_lookup.get(&(end_x + 1, gy, story))
                            && next_node.facade_material == mat
                            && next_node.damage_state == dmg
                        {
                            end_x += 1;
                        } else {
                            break;
                        }
                    }
                    let count = (end_x - start_x + 1).max(1) as usize;
                    let center_x = ((start_x + end_x) as f32) * 0.5 * BUILDING_CELL_STEP_X;
                    let center_y = (gy as f32) * BUILDING_CELL_STEP_Y;
                    spans.push(WallSpan {
                        orientation: WallSpanOrientation::HorizontalX,
                        story_level: story,
                        material: mat,
                        damage_state: dmg,
                        local_center: Vec2::new(center_x, center_y),
                        length: (count as f32) * BUILDING_CELL_STEP_X,
                        width: 15.0,
                        z0,
                        height: story_h,
                        cell_count: count,
                    });
                    gx = end_x + 1;
                } else {
                    gx += 1;
                }
            }
        }

        // 2. Vertical wall runs along West (min_x) and East (max_x) facades (interior of corners)
        let x_cols = if min_x == max_x {
            vec![min_x]
        } else {
            vec![min_x, max_x]
        };
        for gx in x_cols {
            let mut gy = min_y + 1;
            while gy < max_y {
                if let Some(&first_node) = grid_lookup.get(&(gx, gy, story)) {
                    let mat = first_node.facade_material;
                    let dmg = first_node.damage_state;
                    let start_y = gy;
                    let mut end_y = gy;
                    while end_y < max_y - 1 {
                        if let Some(&next_node) = grid_lookup.get(&(gx, end_y + 1, story))
                            && next_node.facade_material == mat
                            && next_node.damage_state == dmg
                        {
                            end_y += 1;
                        } else {
                            break;
                        }
                    }
                    let count = (end_y - start_y + 1).max(1) as usize;
                    let center_x = (gx as f32) * BUILDING_CELL_STEP_X;
                    let center_y = ((start_y + end_y) as f32) * 0.5 * BUILDING_CELL_STEP_Y;
                    spans.push(WallSpan {
                        orientation: WallSpanOrientation::VerticalY,
                        story_level: story,
                        material: mat,
                        damage_state: dmg,
                        local_center: Vec2::new(center_x, center_y),
                        length: 21.0,
                        width: (count as f32) * BUILDING_CELL_STEP_Y,
                        z0,
                        height: story_h,
                        cell_count: count,
                    });
                    gy = end_y + 1;
                } else {
                    gy += 1;
                }
            }
        }
    }

    spans
}

/// Builds a compound Avian2D `Collider` from all intact ground-floor solid cells (`Wall` and
/// `LoadBearingPillar` at `story_level == 0`) so breaching wall cells opens an exact passable
/// doorway into the building while intact cells continue blocking the dozer (`Issue #14`).
#[must_use]
pub fn build_building_compound_collider(
    nodes: &HashMap<NodeIndex, StructuralNode>,
) -> Option<Collider> {
    let mut shapes: Vec<(Position, Rotation, Collider)> = Vec::new();
    let mut sorted_ids: Vec<NodeIndex> = nodes.keys().copied().collect();
    sorted_ids.sort_unstable();

    for id in sorted_ids {
        if let Some(node) = nodes.get(&id)
            && node.is_solid_ground_obstacle()
        {
            let center = (node.cell_min + node.cell_max) * 0.5;
            let size = (node.cell_max - node.cell_min).max(Vec2::splat(4.0));
            shapes.push((
                Position(center),
                Rotation::IDENTITY,
                Collider::rectangle(size.x, size.y),
            ));
        }
    }

    if shapes.is_empty() {
        None
    } else {
        Some(Collider::compound(shapes))
    }
}

/// Returns `true` if a circle `(world_center, radius)` intersects any intact ground-floor solid
/// cell (`Wall` or `LoadBearingPillar` at `story_level == 0`) of the building (`Issue #14`).
#[must_use]
pub fn building_blocks_circle(
    entry: &BuildingGraphEntry,
    building_origin: Vec2,
    world_center: Vec2,
    radius: f32,
) -> bool {
    let safe_c = sanitize_vec2(world_center, Vec2::ZERO);
    let safe_r = sanitize_f32(radius, 0.0).max(0.0);
    let r_sq = safe_r * safe_r;

    for node in entry.nodes.values() {
        if node.is_solid_ground_obstacle() {
            let (w_min, w_max) = node.world_bounds(building_origin);
            let closest = safe_c.clamp(w_min, w_max);
            if safe_c.distance_squared(closest) <= r_sq {
                return true;
            }
        }
    }
    false
}

/// Resource indexing all active building structural graphs on the current site.
#[derive(Resource, Debug, Default)]
pub struct BuildingGraphRegistry {
    pub current_epoch: SessionEpoch,
    /// [`SimTick`] of the tick in progress, synced at `PdSystemSet::SimInput`.
    pub current_tick: u64,
    pub buildings: HashMap<Entity, BuildingGraphEntry>,
}

impl BuildingGraphRegistry {
    pub fn clear_for_epoch(&mut self, new_epoch: SessionEpoch) {
        self.current_epoch = new_epoch;
        self.current_tick = 0;
        self.buildings.clear();
    }

    /// Fills `out` with every building id in stable site order (`site_ordinal`).
    pub fn ordered_building_ids(&self, out: &mut Vec<Entity>) {
        out.clear();
        out.extend(self.buildings.keys().copied());
        out.sort_by_key(|e| self.buildings.get(e).map_or(u32::MAX, |b| b.site_ordinal));
    }
}

/// Ticks between dispatching a structural BFS and applying its result. The result lands on
/// exactly this tick (blocking if the worker has not finished) so replays stay deterministic.
pub const COLLAPSE_EVAL_LATENCY_TICKS: u64 = 1;

/// How often applying a BFS result had to wait for its worker (measures whether
/// [`COLLAPSE_EVAL_LATENCY_TICKS`] is long enough). Diagnostic only; never feeds the sim.
#[derive(Resource, Debug, Clone, Copy, Default, Serialize)]
pub struct CollapseTaskJoinStats {
    pub joins: u64,
    pub blocked_joins: u64,
    pub total_block_micros: u64,
    pub max_block_micros: u64,
}

impl CollapseTaskJoinStats {
    pub fn record(&mut self, was_ready: bool, waited: std::time::Duration) {
        self.joins = self.joins.saturating_add(1);
        if !was_ready {
            let micros = u64::try_from(waited.as_micros()).unwrap_or(u64::MAX);
            self.blocked_joins = self.blocked_joins.saturating_add(1);
            self.total_block_micros = self.total_block_micros.saturating_add(micros);
            self.max_block_micros = self.max_block_micros.max(micros);
        }
    }
}

/// `PdSystemSet::SimInput`: stamps the registry with the tick and epoch now running, so
/// dispatches from `StructuralImpacts` compute their due tick from the current tick.
pub fn sync_graph_registry_clock_system(
    tick: Res<SimTick>,
    epoch: Res<SessionEpoch>,
    mut graph_registry: ResMut<BuildingGraphRegistry>,
) {
    graph_registry.current_tick = tick.0;
    graph_registry.current_epoch = *epoch;
}

/// Pure bounded Breadth-First Search (BFS) that finds all intact nodes no longer connected
/// to any intact foundation node (`is_foundation == true` or `story_level == 0`), accounting
/// for material-dependent horizontal beam spans (`Issue #14` & `Issue #15`).
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

    // Also evaluate local vertical/beam-span support for upper-story nodes (`story_level > 0`):
    // An upper-story node must either be directly above an intact lower-story node OR within
    // `material.beam_span()` horizontal cells of a column/wall that has vertical support down to story 0.
    let mut vertically_supported: HashMap<(i32, i32, u8), bool> =
        HashMap::with_capacity(snapshot.len());
    for node in snapshot.values() {
        if node.intact && node.story_level == 0 && visited.contains(&node.id) {
            vertically_supported.insert((node.grid_x, node.grid_y, 0), true);
        }
    }
    let max_story = snapshot.values().map(|n| n.story_level).max().unwrap_or(0);
    for story in 1..=max_story {
        // Pass 1: direct vertical support from `(gx, gy, story - 1)`
        for node in snapshot.values() {
            if node.intact
                && node.story_level == story
                && visited.contains(&node.id)
                && vertically_supported
                    .get(&(node.grid_x, node.grid_y, story - 1))
                    .copied()
                    .unwrap_or(false)
            {
                vertically_supported.insert((node.grid_x, node.grid_y, story), true);
            }
        }
        // Pass 2: horizontal beam-span bridging (`node.material.beam_span()`) from directly supported nodes on the same story
        let direct_coords: Vec<(i32, i32)> = vertically_supported
            .iter()
            .filter_map(|(&(gx, gy, st), &ok)| if ok && st == story { Some((gx, gy)) } else { None })
            .collect();
        for node in snapshot.values() {
            if node.intact && node.story_level == story && visited.contains(&node.id) {
                let span = i32::from(node.material.beam_span());
                let within_span = direct_coords
                    .iter()
                    .any(|&(sx, sy)| (node.grid_x - sx).abs() + (node.grid_y - sy).abs() <= span);
                if within_span {
                    vertically_supported.insert((node.grid_x, node.grid_y, story), true);
                }
            }
        }
    }

    let mut orphaned_nodes = Vec::new();
    let mut stressed_neighbors = Vec::new();

    if iterations < MAX_GRAPH_ITERATIONS {
        for (&id, node) in snapshot {
            if node.intact {
                let has_support = visited.contains(&id)
                    && (node.story_level == 0
                        || vertically_supported
                            .get(&(node.grid_x, node.grid_y, node.story_level))
                            .copied()
                            .unwrap_or(false));
                if !has_support {
                    orphaned_nodes.push(id);
                }
            }
        }
    }

    // Identify intact nodes adjacent to recently destroyed nodes to apply CrackedStressed visuals.
    for &destroyed_id in recently_destroyed {
        if let Some(d_node) = snapshot.get(&destroyed_id) {
            for &n_id in &d_node.neighbors {
                if let Some(n_node) = snapshot.get(&n_id)
                    && n_node.intact
                    && !orphaned_nodes.contains(&n_id)
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
    let due_tick = graph_registry
        .current_tick
        .saturating_add(COLLAPSE_EVAL_LATENCY_TICKS);

    // 1. Lookup building_id in graph_registry. If missing or already Collapsing, return Skipped.
    let Some(entry) = graph_registry.buildings.get_mut(&building_id) else {
        return EvalStatus::Skipped;
    };
    if entry.collapsing || entry.epoch != epoch {
        return EvalStatus::Skipped;
    }

    // 2. Remove/mark destroyed_nodes in the adjacency graph and increment dirty_generation & visual_revision.
    for &node_id in destroyed_nodes {
        if let Some(node) = entry.nodes.get_mut(&node_id) {
            node.hp = 0.0;
            node.cladding_hp = 0.0;
            node.intact = false;
            node.damage_state = DamageState::RubbleDebris;
        }
    }
    entry.dirty_generation = entry.dirty_generation.saturating_add(1);
    entry.visual_revision = entry.visual_revision.saturating_add(1);
    entry.refresh_surface_cache();

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
    entry.in_flight_due_tick = due_tick;
    entry.pending_recheck = false;
    EvalStatus::Dispatched
}

/// Lifecycle tier for the 4-Layer Debris & PileField system (`Issue #16`).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum DebrisTier {
    #[default]
    ActivePhysics,
    SleepingInstanced,
    CrushedDecal,
}

/// Component attached to spawned debris pieces tracking simulated 2.5D Z-height drop, settling,
/// material mass conservation, and interaction grace timers (`debris.ts` / Issue #16).
#[derive(Component, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DebrisChunk {
    pub tier: DebrisTier,
    pub active_timer_secs: f32,
    pub simulated_z_height: f32,
    pub vertical_z_velocity: f32,
    pub horizontal_velocity: Vec2,
    pub spin_rate: f32,
    pub material: MaterialKind,
    pub mass: f32,
    pub is_remnant: bool,
    pub unattended_secs: f32,
    pub recently_touched_secs: f32,
}

impl Default for DebrisChunk {
    fn default() -> Self {
        Self {
            tier: DebrisTier::ActivePhysics,
            active_timer_secs: 4.0,
            simulated_z_height: 0.0,
            vertical_z_velocity: 0.0,
            horizontal_velocity: Vec2::ZERO,
            spin_rate: 0.0,
            material: MaterialKind::Concrete,
            mass: 1.0,
            is_remnant: false,
            unattended_secs: 0.0,
            recently_touched_secs: 0.0,
        }
    }
}

/// Fixed-capacity ring-buffer pool governing active physics debris (`<= 500` bodies).
#[derive(Resource, Debug, Clone)]
pub struct DebrisPoolRegistry {
    pub active_ring: VecDeque<Entity>,
    pub max_capacity: usize,
    pub total_recycled: u64,
    pub dozer_world_pos: Vec2,
    /// Per-site spawn serial; each chunk gets `SimOrdinal(SIM_ORDINAL_DEBRIS_BASE + serial)`.
    pub next_debris_serial: u64,
}

impl Default for DebrisPoolRegistry {
    fn default() -> Self {
        Self {
            active_ring: VecDeque::with_capacity(MAX_ACTIVE_PHYSICS_DEBRIS),
            max_capacity: MAX_ACTIVE_PHYSICS_DEBRIS,
            total_recycled: 0,
            dozer_world_pos: Vec2::new(0.0, -60.0),
            next_debris_serial: 0,
        }
    }
}

impl DebrisPoolRegistry {
    pub fn clear(&mut self) {
        self.active_ring.clear();
        self.dozer_world_pos = Vec2::new(0.0, -60.0);
        self.next_debris_serial = 0;
    }

    /// Hands out the next stable debris ordinal.
    pub fn next_debris_ordinal(&mut self) -> SimOrdinal {
        let serial = self.next_debris_serial;
        self.next_debris_serial = serial.saturating_add(1);
        SimOrdinal(SIM_ORDINAL_DEBRIS_BASE.saturating_add(serial))
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

/// Helper that generates a multi-story modular building graph with spatial cell bounds and material HP (`Issue #14`).
#[must_use]
pub fn build_procedural_building_graph(stories: u8, footprint_half: i32) -> Vec<StructuralNode> {
    build_rect_building_graph(stories, footprint_half, footprint_half)
}

/// Rectangular variant of [`build_procedural_building_graph`]: `(2 * half_x + 1)` by
/// `(2 * half_y + 1)` cells per story (e.g. the 5x3 ranch is `half_x = 2, half_y = 1`).
#[must_use]
pub fn build_rect_building_graph(stories: u8, half_x: i32, half_y: i32) -> Vec<StructuralNode> {
    let clamped_stories = stories.clamp(1, 8);
    let half_x = half_x.clamp(1, 4);
    let half_y = half_y.clamp(1, 4);
    let mut nodes = Vec::new();
    let mut coord_to_id: HashMap<(i32, i32, u8), NodeIndex> = HashMap::new();

    let default_wall_mat = match clamped_stories {
        1 => MaterialKind::Wood,
        2 | 3 => MaterialKind::Brick,
        _ => MaterialKind::Concrete,
    };
    let pillar_mat = if clamped_stories >= 4 {
        MaterialKind::Steel
    } else if clamped_stories >= 2 {
        MaterialKind::Concrete
    } else {
        MaterialKind::Brick
    };

    let mut next_id: NodeIndex = 0;
    for story in 0..clamped_stories {
        for gy in -half_y..=half_y {
            for gx in -half_x..=half_x {
                let is_perimeter = gx.abs() == half_x || gy.abs() == half_y;
                let is_corner = gx.abs() == half_x && gy.abs() == half_y;
                let kind = if story == clamped_stories - 1 && !is_perimeter {
                    StructuralModuleKind::RoofCorrugation
                } else if is_corner {
                    StructuralModuleKind::LoadBearingPillar
                } else if is_perimeter {
                    StructuralModuleKind::Wall
                } else {
                    StructuralModuleKind::FloorSlab
                };

                let material = match kind {
                    StructuralModuleKind::LoadBearingPillar => pillar_mat,
                    StructuralModuleKind::Wall => default_wall_mat,
                    StructuralModuleKind::FloorSlab => MaterialKind::Concrete,
                    StructuralModuleKind::RoofCorrugation => {
                        if clamped_stories <= 2 {
                            MaterialKind::Wood
                        } else {
                            MaterialKind::Steel
                        }
                    }
                };
                let hp = material.default_hp();
                let cladding_hp = if kind == StructuralModuleKind::LoadBearingPillar {
                    12.0
                } else {
                    0.0
                };
                let (cell_min, cell_max) = StructuralNode::cell_bounds_for_grid(gx, gy);

                let id = next_id;
                next_id = next_id.saturating_add(1);
                coord_to_id.insert((gx, gy, story), id);

                nodes.push(StructuralNode {
                    id,
                    kind,
                    material,
                    facade_material: material,
                    hp,
                    max_hp: hp,
                    cladding_hp,
                    cell_min,
                    cell_max,
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

/// Builds a 2.5D multi-box isometric assembly for a modular building using merged exterior
/// `WallSpan`s and locally supported `RoofBay`s (`Issues #10, #14, and #15`).
#[must_use]
pub fn build_building_iso_assembly_from_entry(
    entry: &BuildingGraphEntry,
    stories: u8,
    palette_index: usize,
) -> (IsoExtrudedBox, IsoBoxAssembly) {
    build_building_iso_assembly_with_spans_and_roofs(
        &entry.nodes,
        &entry.cached_wall_spans,
        &entry.roof_bays,
        stories,
        palette_index,
    )
}

/// Builds a 2.5D multi-box isometric assembly for a modular building from its live
/// `StructuralNode` graph (`Issue #10`, `#14`, `#15`).
#[must_use]
pub fn build_building_iso_assembly(
    nodes: &HashMap<NodeIndex, StructuralNode>,
    stories: u8,
    palette_index: usize,
) -> (IsoExtrudedBox, IsoBoxAssembly) {
    let wall_spans = extract_merged_wall_spans(nodes);
    let roof_style = match stories {
        1 => RoofStyle::Gable,
        2 => RoofStyle::Shed,
        _ => RoofStyle::Flat,
    };
    let roof_bays = build_roof_bays_for_nodes(nodes, roof_style);
    build_building_iso_assembly_with_spans_and_roofs(
        nodes,
        &wall_spans,
        &roof_bays,
        stories,
        palette_index,
    )
}

#[must_use]
pub fn build_building_iso_assembly_with_spans_and_roofs(
    nodes: &HashMap<NodeIndex, StructuralNode>,
    wall_spans: &[WallSpan],
    roof_bays: &[RoofBay],
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

    let mut parts = Vec::with_capacity(sorted_ids.len() + wall_spans.len() + roof_bays.len() + 4);
    let story_h = 16.0_f32;

    // 1. Add merged exterior wall spans (`Issue #15`)
    for span in wall_spans {
        let is_stressed = span.damage_state == DamageState::CrackedStressed;
        let base_c = if is_stressed {
            let s = wall_col.to_srgba();
            Color::srgba(s.red * 0.68, s.green * 0.64, s.blue * 0.60, s.alpha)
        } else {
            wall_col
        };
        parts.push(
            IsoExtrudedBox::new(span.length, span.width, span.height, base_c)
                .with_offset(span.local_center, span.z0)
                .with_shadow(false),
        );
    }

    // 2. Add pillars, floor slabs, and window accents from intact nodes
    for id in sorted_ids {
        let Some(node) = nodes.get(&id) else {
            continue;
        };
        if !node.intact {
            continue;
        }

        let local_xy = Vec2::new(
            node.grid_x as f32 * BUILDING_CELL_STEP_X,
            node.grid_y as f32 * BUILDING_CELL_STEP_Y,
        );
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
                parts.push(
                    IsoExtrudedBox::new(21.5, 15.5, 2.0, tint(trim_col))
                        .with_offset(local_xy, z0 + story_h - 2.0)
                        .with_shadow(false),
                );
            }
            StructuralModuleKind::Wall => {
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
                if node.grid_x == 0 && node.grid_y == 0 && roof_bays.is_empty() {
                    parts.push(
                        IsoExtrudedBox::new(21.4, 15.4, 5.5, tint(roof_col))
                            .with_offset(local_xy, z0 + story_h - 4.5)
                            .with_shadow(false),
                    );
                }
            }
        }
    }

    // 3. Add locally supported roof bays (`Gable`, `Shed`, `Flat`) including sagging/hinging pose (`Issue #15`)
    for bay in roof_bays {
        if bay.state == RoofBayState::Collapsed {
            continue;
        }
        let bay_center = (bay.cell_min + bay.cell_max) * 0.5;
        let bay_size = (bay.cell_max - bay.cell_min).max(Vec2::new(20.0, 20.0));
        let (sag_z, pitch, roll, col) = match bay.state {
            RoofBayState::Intact => (0.0, 0.0, 0.0, roof_col),
            RoofBayState::Sagging {
                progress,
                hinge_dir,
            } => {
                let p = sanitize_f32(progress, 0.0).clamp(0.0, 1.0);
                let dir = sanitize_vec2(hinge_dir, Vec2::NEG_Y);
                let s = roof_col.to_srgba();
                let sag_col = Color::srgba(s.red * 0.72, s.green * 0.68, s.blue * 0.64, s.alpha);
                (-p * 9.5, dir.x * p * 0.45, dir.y * p * 0.45, sag_col)
            }
            RoofBayState::Collapsed => continue,
        };

        let mut roof_box = IsoExtrudedBox::new(bay_size.x, bay_size.y, bay.height, col)
            .with_offset(bay_center, (bay.z_base + sag_z).max(2.0))
            .with_pitch_roll(pitch, roll)
            .with_shadow(false);
        match bay.style {
            RoofStyle::Gable => {
                roof_box = roof_box.with_roof_slope(RoofSlopeSpec::Gable {
                    eave_scale: 0.55,
                    ridge_scale: 1.25,
                });
            }
            RoofStyle::Shed => {
                roof_box = roof_box.with_roof_slope(RoofSlopeSpec::AcrossWidth {
                    low_scale: 0.65,
                    high_scale: 1.20,
                });
            }
            RoofStyle::Flat => {}
        }
        parts.push(roof_box);

        if bay.bay_id == 1 && bay.state == RoofBayState::Intact && stories >= 2 {
            parts.push(
                IsoExtrudedBox::new(12.0, 10.0, 6.5, Color::srgb(0.62, 0.66, 0.70))
                    .with_offset(bay_center, bay.z_base + bay.height)
                    .with_shadow(false),
            );
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

/// Builds a 2.5D multi-box assembly for interior furniture and exterior street props (`Issues #10 and #17`).
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
            IsoExtrudedBox::new(22.0, 7.0, 9.5, Color::srgb(0.66, 0.65, 0.62)).with_roof_slope(
                RoofSlopeSpec::Gable {
                    eave_scale: 0.70,
                    ridge_scale: 1.15,
                },
            )
        }
        PropArchetype::UtilityPole => {
            parts.push(
                IsoExtrudedBox::new(3.5, 14.0, 2.2, Color::srgb(0.40, 0.32, 0.24))
                    .with_offset(Vec2::ZERO, 24.0)
                    .with_shadow(false),
            );
            IsoExtrudedBox::new(4.2, 4.2, 26.0, Color::srgb(0.48, 0.36, 0.26))
        }
        PropArchetype::PineTree => {
            parts.push(
                IsoExtrudedBox::new(14.0, 14.0, 14.0, Color::srgb(0.20, 0.50, 0.24))
                    .with_offset(Vec2::ZERO, 8.0)
                    .with_roof_slope(RoofSlopeSpec::Gable {
                        eave_scale: 0.35,
                        ridge_scale: 1.35,
                    })
                    .with_shadow(false),
            );
            IsoExtrudedBox::new(4.5, 4.5, 8.5, Color::srgb(0.46, 0.32, 0.20))
        }
        PropArchetype::HayBale | PropArchetype::FuelDrum => {
            IsoExtrudedBox::new(11.0, 11.0, 9.5, archetype.color())
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

/// Spawns a single dynamic debris entity with explicit material, mass, and remnant flag (`Issue #16`).
#[allow(clippy::too_many_arguments)]
pub fn spawn_pooled_debris_chunk_with_material(
    commands: &mut Commands,
    debris_pool: &mut DebrisPoolRegistry,
    epoch: SessionEpoch,
    world_pos: Vec2,
    story_level: u8,
    material: MaterialKind,
    mass: f32,
    is_remnant: bool,
    initial_horiz_vel: Option<Vec2>,
) -> Entity {
    let safe_pos = sanitize_vec2(world_pos, Vec2::ZERO);
    let safe_mass = sanitize_f32(mass, 1.0).clamp(0.1, 20.0);
    let sim_z = f32::from(story_level) * 18.0 + 6.0;
    let iso_screen = world_to_iso_screen(safe_pos, sim_z);
    let iso_z = iso_depth_key(safe_pos, sim_z, ISO_BIAS_DEBRIS);
    let (mut base_box, assembly) = build_debris_iso_assembly(story_level);
    base_box.base_color = material.rubble_color();
    if is_remnant {
        base_box.length = 18.0;
        base_box.width = 12.0;
        base_box.height = 5.0;
    }

    let angle = (safe_pos.x * 0.13 + safe_pos.y * 0.29 + f32::from(story_level) * 1.7).sin()
        * std::f32::consts::PI;
    let horiz_vel = initial_horiz_vel
        .map(|v| sanitize_vec2(v, Vec2::ZERO))
        .unwrap_or_else(|| Vec2::new(angle.cos() * 24.0, angle.sin() * 24.0));

    let ordinal = debris_pool.next_debris_ordinal();
    let debris_entity = commands
        .spawn((
            Name::new("Falling Structural Debris"),
            SiteScoped { epoch },
            ordinal,
            DebrisTier::ActivePhysics,
            DebrisChunk {
                tier: DebrisTier::ActivePhysics,
                active_timer_secs: if is_remnant { 5.0 } else { 4.0 },
                simulated_z_height: sim_z,
                vertical_z_velocity: -65.0,
                horizontal_velocity: horiz_vel,
                spin_rate: if is_remnant { 1.4 } else { 2.2 },
                material,
                mass: safe_mass,
                is_remnant,
                unattended_secs: 0.0,
                recently_touched_secs: 0.5,
            },
            WorldPose::new(safe_pos, sim_z, angle),
            IsoLayerBias(ISO_BIAS_DEBRIS),
            base_box,
            assembly,
            Transform::from_xyz(iso_screen.x, iso_screen.y, iso_z),
            GlobalTransform::default(),
            (
                Position(safe_pos),
                Rotation::radians(angle),
                RigidBody::Dynamic,
                Collider::rectangle(base_box.length, base_box.width),
                LinearVelocity::default(),
                AngularVelocity::default(),
            ),
        ))
        .id();

    debris_pool.register_or_recycle_oldest(debris_entity, commands);
    debris_entity
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
    spawn_pooled_debris_chunk_with_material(
        commands,
        debris_pool,
        epoch,
        world_pos,
        story_level,
        MaterialKind::Concrete,
        1.2,
        false,
        None,
    )
}

/// Applies localized damage or destruction to a `DestructibleProp` according to its
/// 7-profile `AssetCatalogDef` (`Brittle`, `BendSnap`, `Crush`, `Topple`, `Roll`,
/// `PanelCollapse`, `Explosive`) and deposits conserved mass into `PileField` (`Issue #17`).
///
/// Returns `(destroyed, track_stress_delta, queued_explosion_opt)`.
#[allow(clippy::too_many_arguments)]
pub fn apply_prop_damage_and_profile(
    commands: &mut Commands,
    epoch: SessionEpoch,
    prop_entity: Entity,
    prop_pos: Vec2,
    prop: &mut DestructibleProp,
    prop_pose: Option<&mut WorldPose>,
    prop_box: Option<&mut IsoExtrudedBox>,
    raw_damage: f32,
    impact_dir: Vec2,
    blade_down: bool,
    debris_pool: &mut DebrisPoolRegistry,
    pile_field: &mut PileField,
    cash_events: &mut EventWriter<DemolitionCashEvent>,
    decal_events: &mut EventWriter<DecalSpawnRequest>,
    particle_events: &mut EventWriter<ParticleBurstEvent>,
    audio_events: &mut EventWriter<AudioTriggerEvent>,
) -> (bool, f32, Option<(Vec2, f32, f32, u8)>) {
    let def = prop.archetype.catalog_def();
    let safe_pos = sanitize_vec2(prop_pos, Vec2::ZERO);
    let safe_dir = sanitize_vec2(impact_dir, Vec2::X).normalize_or_zero();
    let dir = if safe_dir.length_squared() > 1e-4 {
        safe_dir
    } else {
        Vec2::X
    };

    let track_stress_delta = if blade_down {
        0.0
    } else {
        def.track_hazard_stress
    };

    let scaled_dmg = sanitize_f32(raw_damage * def.blade_damage_mul, 0.0).max(0.0);
    prop.hp = sanitize_f32(prop.hp - scaled_dmg, 0.0).max(0.0);
    let dmg_ratio = (1.0 - (prop.hp / prop.max_hp.max(1.0))).clamp(0.0, 1.0);
    prop.lean_dir = dir;

    match def.profile {
        DestructionProfile::BendSnap | DestructionProfile::Topple => {
            prop.lean_progress = dmg_ratio;
            if let Some(pose) = prop_pose {
                pose.pitch_rad = dir.x * dmg_ratio * 0.65;
                pose.roll_rad = dir.y * dmg_ratio * 0.65;
            }
        }
        DestructionProfile::Crush => {
            prop.crush_progress = dmg_ratio;
            if let Some(b) = prop_box {
                b.height = (b.height * (1.0 - dmg_ratio * 0.45)).max(2.5);
            }
        }
        _ => {}
    }

    if prop.hp > 0.0 {
        return (false, track_stress_delta, None);
    }

    // Prop reached hp <= 0.0: execute profile-specific destruction
    let p_screen = world_to_iso_screen(safe_pos, 4.0);
    cash_events.send(DemolitionCashEvent {
        amount: def.cash_value,
        world_pos: p_screen.extend(0.0),
    });

    let mut queued_blast = None;

    match def.profile {
        DestructionProfile::Brittle => {
            pile_field.add_mass(safe_pos, def.mass, def.material);
            if matches!(
                prop.archetype,
                PropArchetype::Toilet | PropArchetype::FireHydrant
            ) {
                particle_events.send(ParticleBurstEvent {
                    kind: ParticleBurstKind::WaterSpray,
                    world_pos: safe_pos,
                    count: 10,
                });
                audio_events.send(AudioTriggerEvent {
                    kind: AudioSfxKind::BladeHydraulicHiss,
                });
            } else if prop.archetype == PropArchetype::CashRegister {
                audio_events.send(AudioTriggerEvent {
                    kind: AudioSfxKind::CashDing,
                });
            } else {
                particle_events.send(ParticleBurstEvent {
                    kind: ParticleBurstKind::MasonryDust,
                    world_pos: safe_pos,
                    count: 6,
                });
                audio_events.send(AudioTriggerEvent {
                    kind: AudioSfxKind::StructuralCrunch,
                });
            }
        }
        DestructionProfile::BendSnap => {
            pile_field.add_mass(safe_pos, def.mass * 0.45, def.material);
            spawn_pooled_debris_chunk_with_material(
                commands,
                debris_pool,
                epoch,
                safe_pos + dir * 6.0,
                0,
                def.material,
                def.mass * 0.55,
                true,
                Some(dir * 32.0),
            );
            if prop.archetype == PropArchetype::FireHydrant {
                particle_events.send(ParticleBurstEvent {
                    kind: ParticleBurstKind::WaterSpray,
                    world_pos: safe_pos,
                    count: 10,
                });
                audio_events.send(AudioTriggerEvent {
                    kind: AudioSfxKind::BladeHydraulicHiss,
                });
            } else {
                audio_events.send(AudioTriggerEvent {
                    kind: AudioSfxKind::StructuralCrunch,
                });
            }
        }
        DestructionProfile::Crush => {
            pile_field.add_mass(safe_pos, def.mass, def.material);
            decal_events.send(DecalSpawnRequest {
                kind: DecalKind::CrushedRubbleScar,
                world_pos: safe_pos,
                size: Vec2::new(18.0, 12.0),
                rotation_rad: 0.0,
            });
            audio_events.send(AudioTriggerEvent {
                kind: AudioSfxKind::StructuralCrunch,
            });
        }
        DestructionProfile::Topple => {
            spawn_pooled_debris_chunk_with_material(
                commands,
                debris_pool,
                epoch,
                safe_pos + dir * 10.0,
                0,
                def.material,
                def.mass,
                true,
                Some(dir * 28.0),
            );
            audio_events.send(AudioTriggerEvent {
                kind: AudioSfxKind::StructuralCrunch,
            });
        }
        DestructionProfile::Roll => {
            let roll_ent = spawn_pooled_debris_chunk_with_material(
                commands,
                debris_pool,
                epoch,
                safe_pos + dir * 8.0,
                0,
                def.material,
                def.mass,
                true,
                Some(dir * 52.0),
            );
            if let Some(mut e_cmds) = commands.get_entity(roll_ent) {
                e_cmds.insert(DebrisChunk {
                    tier: DebrisTier::ActivePhysics,
                    active_timer_secs: 5.5,
                    simulated_z_height: 2.0,
                    vertical_z_velocity: -20.0,
                    horizontal_velocity: dir * 52.0,
                    spin_rate: 6.5,
                    material: def.material,
                    mass: def.mass,
                    is_remnant: true,
                    unattended_secs: 0.0,
                    recently_touched_secs: 0.8,
                });
            }
            audio_events.send(AudioTriggerEvent {
                kind: AudioSfxKind::StructuralCrunch,
            });
        }
        DestructionProfile::PanelCollapse => {
            let half_mass = (def.mass * 0.5).max(0.2);
            for side in [-1.0_f32, 1.0_f32] {
                let perp = Vec2::new(-dir.y * side, dir.x * side);
                spawn_pooled_debris_chunk_with_material(
                    commands,
                    debris_pool,
                    epoch,
                    safe_pos + perp * 6.0,
                    0,
                    def.material,
                    half_mass,
                    true,
                    Some(dir * 26.0 + perp * 12.0),
                );
            }
            audio_events.send(AudioTriggerEvent {
                kind: AudioSfxKind::StructuralCrunch,
            });
        }
        DestructionProfile::Explosive { max_generation } => {
            pile_field.add_mass(safe_pos, def.mass, def.material);
            particle_events.send(ParticleBurstEvent {
                kind: ParticleBurstKind::PropaneExplosion,
                world_pos: safe_pos,
                count: 14,
            });
            decal_events.send(DecalSpawnRequest {
                kind: DecalKind::ScorchMark,
                world_pos: safe_pos,
                size: Vec2::new(42.0, 32.0),
                rotation_rad: 0.0,
            });
            audio_events.send(AudioTriggerEvent {
                kind: AudioSfxKind::PropaneBlast,
            });
            if prop.chain_generation <= max_generation {
                queued_blast = Some((
                    safe_pos,
                    def.blast_radius,
                    def.blast_damage,
                    prop.chain_generation,
                ));
            }
        }
    }

    if let Some(e_cmds) = commands.get_entity(prop_entity) {
        e_cmds.despawn_recursive();
    }

    (true, track_stress_delta, queued_blast)
}

/// Polls in-flight async BFS structural collapse tasks, validates `SessionEpoch` and
/// `dirty_generation`, converts orphaned upper stories into falling debris clusters, and
/// rebuilds the building's 2.5D `IsoBoxAssembly` and per-cell compound `Collider`.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn poll_structural_collapse_tasks_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    tick: Res<SimTick>,
    mut join_stats: ResMut<CollapseTaskJoinStats>,
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
    graph_registry.current_tick = tick.0;
    let current_tick = tick.0;

    let task_pool = AsyncComputeTaskPool::get();
    graph_registry.ordered_building_ids(&mut building_ids);

    for &building_id in building_ids.iter() {
        let mut completed_result: Option<CollapseEvalResult> = None;
        let mut needs_followup = false;

        // Fixed-latency join: apply on exactly the due tick, blocking if the worker is late.
        if let Some(entry) = graph_registry.buildings.get_mut(&building_id)
            && current_tick >= entry.in_flight_due_tick
            && let Some(task) = entry.in_flight_task.take()
        {
            let was_ready = task.is_finished();
            let started = std::time::Instant::now();
            let res = block_on(task);
            join_stats.record(was_ready, started.elapsed());
            completed_result = Some(res);
            needs_followup = entry.pending_recheck;
        }

        if let Some(res) = completed_result {
            if res.epoch != current_epoch {
                continue;
            }
            if commands.get_entity(building_id).is_none() {
                continue;
            }

            let base_pos = building_query
                .get(building_id)
                .map(|(pose, tf, _, _, _)| {
                    pose.map_or_else(|| tf.translation.truncate(), |p| p.pos)
                })
                .unwrap_or(Vec2::ZERO);

            if let Some(entry) = graph_registry.buildings.get_mut(&building_id)
                && (entry.dirty_generation == res.generation || !needs_followup)
            {
                let mut changed = false;
                for &stressed_id in &res.stressed_neighbors {
                    if let Some(node) = entry.nodes.get_mut(&stressed_id)
                        && node.intact
                        && node.damage_state == DamageState::Intact
                    {
                        node.damage_state = DamageState::CrackedStressed;
                        changed = true;
                    }
                }

                if !res.orphaned_nodes.is_empty() {
                    changed = true;
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
                        node.hp = 0.0;
                        node.cladding_hp = 0.0;
                        node.intact = false;
                        node.damage_state = DamageState::RubbleDebris;
                        let world_xy = node.world_center(base_pos);
                        let screen_xy =
                            world_to_iso_screen(world_xy, f32::from(node.story_level) * 16.0);

                        cash_events.send(DemolitionCashEvent {
                            amount: node.cash_value,
                            world_pos: screen_xy.extend(0.0),
                        });

                        spawn_pooled_debris_chunk_with_material(
                            &mut commands,
                            &mut debris_pool,
                            current_epoch,
                            world_xy,
                            node.story_level,
                            node.material,
                            node.material.density(),
                            false,
                            None,
                        );
                    }
                }

                if changed {
                    entry.visual_revision = entry.visual_revision.saturating_add(1);
                    entry.refresh_surface_cache();

                    if let Some(mut b_cmds) = commands.get_entity(building_id) {
                        if let Some(new_col) = build_building_compound_collider(&entry.nodes) {
                            b_cmds.insert(new_col);
                        } else {
                            b_cmds
                                .remove::<RigidBody>()
                                .remove::<Collider>()
                                .remove::<Position>()
                                .remove::<Rotation>();
                        }
                    }

                    if let Ok((_, _, b_meta, Some(mut base_box), Some(mut assembly))) =
                        building_query.get_mut(building_id)
                    {
                        let prev_alpha = assembly.alpha_multiplier;
                        let (mut new_base, mut new_asm) = build_building_iso_assembly_from_entry(
                            entry,
                            b_meta.stories,
                            b_meta.palette_index,
                        );
                        new_base.alpha = prev_alpha;
                        new_asm.alpha_multiplier = prev_alpha;
                        *base_box = new_base;
                        *assembly = new_asm;
                    }
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

/// Evaluates locally supported roof bays (`< 55%` bay support triggers sag/hinge, `< 35%` global
/// support drops remaining non-industrial roofs), applies domino lean impacts to adjacent buildings
/// when a roof bay collapses, and transitions buildings at `>= 90%` settled destruction into
/// `CollapsedSite` scars with a single full-building bonus (`Issues #15 and #16`).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn step_building_roofs_and_collapse_sites_system(
    mut commands: Commands,
    time: Res<Time>,
    epoch: Res<SessionEpoch>,
    mut graph_registry: ResMut<BuildingGraphRegistry>,
    mut debris_pool: ResMut<DebrisPoolRegistry>,
    mut pile_field: ResMut<PileField>,
    mut cash_events: EventWriter<DemolitionCashEvent>,
    mut decal_events: EventWriter<DecalSpawnRequest>,
    mut particle_events: EventWriter<ParticleBurstEvent>,
    mut audio_events: EventWriter<AudioTriggerEvent>,
    mut building_ids: Local<Vec<Entity>>,
    mut domino_impacts: Local<Vec<(Vec2, Vec2, f32)>>,
    mut building_query: Query<(
        Option<&WorldPose>,
        &Transform,
        &ModularBuilding,
        Option<&mut IsoExtrudedBox>,
        Option<&mut IsoBoxAssembly>,
    )>,
) {
    let Ok(dt) = sanitize_dt(time.delta_secs()) else {
        return;
    };
    let current_epoch = *epoch;
    let task_pool = AsyncComputeTaskPool::get();

    graph_registry.ordered_building_ids(&mut building_ids);
    domino_impacts.clear();

    for &building_id in building_ids.iter() {
        if commands.get_entity(building_id).is_none() {
            continue;
        }
        let base_pos = building_query
            .get(building_id)
            .map(|(pose, tf, _, _, _)| pose.map_or_else(|| tf.translation.truncate(), |p| p.pos))
            .unwrap_or(Vec2::ZERO);

        let mut assembly_dirty = false;

        if let Some(entry) = graph_registry.buildings.get_mut(&building_id) {
            if entry.epoch != current_epoch {
                continue;
            }

            // Compute global support ratio across all roof bays
            let mut global_total_sup = 0_usize;
            let mut global_intact_sup = 0_usize;
            for bay in &entry.roof_bays {
                for &sup_id in &bay.support_nodes {
                    global_total_sup = global_total_sup.saturating_add(1);
                    if entry.nodes.get(&sup_id).is_some_and(|n| n.intact) {
                        global_intact_sup = global_intact_sup.saturating_add(1);
                    }
                }
            }
            let global_ratio = if global_total_sup > 0 {
                global_intact_sup as f32 / global_total_sup as f32
            } else {
                1.0
            };

            let mut collapsed_roof_nodes: Vec<NodeIndex> = Vec::new();

            for bay in &mut entry.roof_bays {
                match bay.state {
                    RoofBayState::Intact => {
                        let total_sup = bay.support_nodes.len();
                        if total_sup == 0 {
                            continue;
                        }
                        let mut intact_sup = 0_usize;
                        let mut intact_centroid = Vec2::ZERO;
                        let mut missing_centroid = Vec2::ZERO;
                        let mut missing_sup = 0_usize;

                        for &sup_id in &bay.support_nodes {
                            if let Some(n) = entry.nodes.get(&sup_id) {
                                let c = (n.cell_min + n.cell_max) * 0.5;
                                if n.intact {
                                    intact_sup = intact_sup.saturating_add(1);
                                    intact_centroid += c;
                                } else {
                                    missing_sup = missing_sup.saturating_add(1);
                                    missing_centroid += c;
                                }
                            }
                        }

                        let bay_ratio = intact_sup as f32 / total_sup as f32;
                        let global_trigger = bay.style != RoofStyle::Flat && global_ratio < 0.35;
                        if bay_ratio < 0.55 || global_trigger {
                            let hinge_dir = if missing_sup > 0 && intact_sup > 0 {
                                let m_avg = missing_centroid / (missing_sup as f32);
                                let i_avg = intact_centroid / (intact_sup as f32);
                                sanitize_vec2((m_avg - i_avg).normalize_or_zero(), Vec2::NEG_Y)
                            } else {
                                Vec2::NEG_Y
                            };
                            bay.state = RoofBayState::Sagging {
                                progress: 0.08,
                                hinge_dir: if hinge_dir.length_squared() > 1e-4 {
                                    hinge_dir
                                } else {
                                    Vec2::NEG_Y
                                },
                            };
                            assembly_dirty = true;
                        }
                    }
                    RoofBayState::Sagging {
                        mut progress,
                        hinge_dir,
                    } => {
                        progress = sanitize_f32(progress + dt * 1.85, 1.0).clamp(0.0, 1.0);
                        assembly_dirty = true;
                        if progress >= 1.0 {
                            bay.state = RoofBayState::Collapsed;
                            collapsed_roof_nodes.extend(bay.roof_nodes.iter().copied());
                            let bay_world = base_pos + (bay.cell_min + bay.cell_max) * 0.5;
                            let safe_dir =
                                sanitize_vec2(hinge_dir, Vec2::NEG_Y).normalize_or_zero();

                            // Spawn pushable roof panel remnant (`Issue #15`)
                            spawn_pooled_debris_chunk_with_material(
                                &mut commands,
                                &mut debris_pool,
                                current_epoch,
                                bay_world,
                                1,
                                bay.material,
                                bay.mass,
                                true,
                                Some(safe_dir * 30.0),
                            );
                            particle_events.send(ParticleBurstEvent {
                                kind: ParticleBurstKind::MasonryDust,
                                world_pos: bay_world,
                                count: 8,
                            });
                            audio_events.send(AudioTriggerEvent {
                                kind: AudioSfxKind::CollapseRumble,
                            });
                            domino_impacts.push((bay_world, safe_dir, 48.0));
                        } else {
                            bay.state = RoofBayState::Sagging {
                                progress,
                                hinge_dir,
                            };
                        }
                    }
                    RoofBayState::Collapsed => {}
                }
            }

            // Mark collapsed roof nodes as rubble and award per-node roof cash (never a duplicate full-building bonus)
            for r_id in collapsed_roof_nodes {
                if let Some(node) = entry.nodes.get_mut(&r_id)
                    && node.intact
                {
                    node.hp = 0.0;
                    node.intact = false;
                    node.damage_state = DamageState::RubbleDebris;
                    let w_xy = node.world_center(base_pos);
                    let s_xy = world_to_iso_screen(w_xy, f32::from(node.story_level) * 16.0);
                    cash_events.send(DemolitionCashEvent {
                        amount: node.cash_value,
                        world_pos: s_xy.extend(0.0),
                    });
                }
            }

            // Check >= 90% settled demolition transition into `CollapsedSite` (`Issue #16`)
            if entry.destroyed_fraction() >= 0.90 && entry.in_flight_task.is_none() {
                // Ensure any remaining roof bays are marked collapsed
                for bay in &mut entry.roof_bays {
                    if bay.state != RoofBayState::Collapsed {
                        bay.state = RoofBayState::Collapsed;
                        assembly_dirty = true;
                    }
                }
                if !entry.collapse_bonus_paid {
                    entry.collapse_bonus_paid = true;
                    let screen_xy = world_to_iso_screen(base_pos, 12.0);
                    cash_events.send(DemolitionCashEvent {
                        amount: entry.full_collapse_bonus,
                        world_pos: screen_xy.extend(0.0),
                    });
                }
                if !entry.collapsed_site_spawned {
                    entry.collapsed_site_spawned = true;
                    pile_field.add_mass(base_pos, 12.0, MaterialKind::Concrete);
                    decal_events.send(DecalSpawnRequest {
                        kind: DecalKind::ConcreteFoundationScar,
                        world_pos: base_pos,
                        size: Vec2::new(96.0, 68.0),
                        rotation_rad: 0.0,
                    });
                    if let Some(mut b_cmds) = commands.get_entity(building_id) {
                        b_cmds
                            .remove::<RigidBody>()
                            .remove::<Collider>()
                            .remove::<Position>()
                            .remove::<Rotation>()
                            .insert(CollapsedSite {
                                footprint_half_extents: Vec2::new(54.0, 38.0),
                                dominant_material: MaterialKind::Concrete,
                                rubble_mass: 12.0,
                            });
                    }
                    assembly_dirty = true;
                }
            }

            if assembly_dirty {
                entry.visual_revision = entry.visual_revision.saturating_add(1);
                entry.refresh_surface_cache();
                if let Ok((_, _, b_meta, Some(mut base_box), Some(mut assembly))) =
                    building_query.get_mut(building_id)
                {
                    let prev_alpha = assembly.alpha_multiplier;
                    let (mut new_base, mut new_asm) = build_building_iso_assembly_from_entry(
                        entry,
                        b_meta.stories,
                        b_meta.palette_index,
                    );
                    new_base.alpha = prev_alpha;
                    new_asm.alpha_multiplier = prev_alpha;
                    *base_box = new_base;
                    *assembly = new_asm;
                }
            }
        }
    }

    // Apply domino roof-fall lean impacts to adjacent buildings within range (`Issue #15`)
    for (impact_origin, hinge_dir, dmg) in domino_impacts.iter().copied() {
        let reach_point = impact_origin + hinge_dir * 75.0;
        for &target_id in building_ids.iter() {
            let target_pos = building_query
                .get(target_id)
                .map(|(pose, tf, _, _, _)| {
                    pose.map_or_else(|| tf.translation.truncate(), |p| p.pos)
                })
                .unwrap_or(Vec2::splat(99999.0));
            if target_pos.distance_squared(impact_origin) < 16.0
                || target_pos.distance_squared(reach_point) > 135.0 * 135.0
            {
                continue;
            }
            let mut newly_destroyed = Vec::new();
            if let Some(entry) = graph_registry.buildings.get_mut(&target_id) {
                for (&nid, node) in &mut entry.nodes {
                    if node.intact
                        && node.world_center(target_pos).distance_squared(reach_point)
                            <= 64.0 * 64.0
                        && node.apply_localized_damage(dmg)
                    {
                        newly_destroyed.push(nid);
                    }
                }
                if !newly_destroyed.is_empty() {
                    entry.visual_revision = entry.visual_revision.saturating_add(1);
                    entry.refresh_surface_cache();
                }
            }
            if !newly_destroyed.is_empty() {
                let _ = queue_structural_integrity_evaluation(
                    target_id,
                    &newly_destroyed,
                    &mut graph_registry,
                    task_pool,
                );
            }
        }
    }
}

/// Updates the 4-Layer Debris Lifecycle in Cartesian `WorldPose` space (`Issue #16`):
/// - Tier 1 (`ActivePhysics`) -> Tier 2 (`SleepingInstanced`) once velocity settles or timer expires
/// - Elevates resting rubble by `pile_field.height_at(pos)`
/// - Absorbs excess/unattended distant rubble into `PileField` while strictly conserving mass
///   and never despawning chunks near or recently touched by the dozer.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_debris_lifecycle_system(
    mut commands: Commands,
    time: Res<Time>,
    mut road_graph: ResMut<RoadSplineGraph>,
    mut debris_pool: ResMut<DebrisPoolRegistry>,
    mut pile_field: Option<ResMut<PileField>>,
    graph_registry: Res<BuildingGraphRegistry>,
    mut telemetry: ResMut<DebugTelemetry>,
    mut cleanup_candidates: Local<Vec<(Entity, Vec2, f32, MaterialKind, f32, u64)>>,
    mut debris_query: Query<(
        Entity,
        &mut DebrisChunk,
        Option<&mut WorldPose>,
        Option<&mut Position>,
        Option<&mut Rotation>,
        &mut Transform,
        Option<&LinearVelocity>,
        Option<&SimOrdinal>,
    )>,
) {
    let Ok(dt) = sanitize_dt(time.delta_secs()) else {
        return;
    };

    let mut active_count: usize = 0;
    let mut sleeping_count: usize = 0;
    cleanup_candidates.clear();

    let dozer_pos = sanitize_vec2(debris_pool.dozer_world_pos, Vec2::new(0.0, -60.0));

    for (
        entity,
        mut chunk,
        mut world_pose,
        mut phys_pos,
        mut phys_rot,
        mut transform,
        lin_vel,
        ordinal,
    ) in &mut debris_query
    {
        chunk.unattended_secs = sanitize_f32(chunk.unattended_secs + dt, 0.0);
        chunk.recently_touched_secs =
            sanitize_f32(chunk.recently_touched_secs - dt, 0.0).max(0.0);

        match chunk.tier {
            DebrisTier::ActivePhysics => {
                active_count = active_count.saturating_add(1);
                chunk.active_timer_secs -= dt;

                if let (Some(pose), Some(pos)) = (world_pose.as_deref_mut(), phys_pos.as_deref()) {
                    pose.pos = sanitize_vec2(pos.0, pose.pos);
                }

                let ground_h = pile_field
                    .as_deref()
                    .map_or(0.0, |pf| {
                        let p = world_pose
                            .as_deref()
                            .map_or_else(|| transform.translation.truncate(), |wp| wp.pos);
                        pf.height_at(p)
                    });

                if chunk.simulated_z_height > ground_h {
                    chunk.vertical_z_velocity =
                        sanitize_f32(chunk.vertical_z_velocity - 180.0 * dt, -65.0);
                    let dz = chunk.vertical_z_velocity * dt;
                    chunk.simulated_z_height = (chunk.simulated_z_height + dz).max(ground_h);

                    if let Some(pose) = world_pose.as_deref_mut() {
                        pose.pos =
                            sanitize_vec2(pose.pos + chunk.horizontal_velocity * dt, pose.pos);
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
                    pose.elev_z = ground_h;
                    pose.pitch_rad = 0.0;
                    let drag = (4.5 + chunk.material.friction() * 3.0).clamp(3.0, 9.0);
                    chunk.horizontal_velocity *= (1.0 - dt * drag).clamp(0.0, 1.0);
                    pose.pos = sanitize_vec2(pose.pos + chunk.horizontal_velocity * dt, pose.pos);
                }

                if let Some(pose) = world_pose.as_deref() {
                    if let Some(pos) = phys_pos.as_deref_mut() {
                        pos.0 = pose.pos;
                    }
                    if let Some(rot) = phys_rot.as_deref_mut() {
                        *rot = Rotation::radians(pose.heading_rad);
                    }
                }

                let custom_speed_sq =
                    sanitize_vec2(chunk.horizontal_velocity, Vec2::ZERO).length_squared();
                let solver_speed_sq =
                    lin_vel.map_or(0.0, |v| sanitize_vec2(v.0, Vec2::ZERO).length_squared());
                let speed_sq = custom_speed_sq.max(solver_speed_sq);
                let settled = chunk.active_timer_secs <= 0.0
                    || (chunk.simulated_z_height <= ground_h + 0.05 && speed_sq < 4.0);

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
                            .remove::<Position>()
                            .remove::<Rotation>()
                            .remove::<LinearVelocity>()
                            .remove::<AngularVelocity>()
                            .insert(DebrisTier::SleepingInstanced);
                    }
                }
            }
            DebrisTier::SleepingInstanced => {
                sleeping_count = sleeping_count.saturating_add(1);
                let world_xy = if let Some(pose) = world_pose.as_deref_mut() {
                    if let Some(pf) = pile_field.as_deref() {
                        pose.elev_z = pf.height_at(pose.pos);
                    }
                    pose.pos
                } else {
                    transform.translation.truncate()
                };

                let dist_sq = world_xy.distance_squared(dozer_pos);
                if chunk.recently_touched_secs <= 0.0 && dist_sq > 56.0 * 56.0 {
                    let priority = dist_sq / if chunk.is_remnant { 1.8 } else { 1.0 };
                    cleanup_candidates.push((
                        entity,
                        world_xy,
                        chunk.mass,
                        chunk.material,
                        priority,
                        ordinal.map_or(u64::MAX, |o| o.0),
                    ));
                }
            }
            DebrisTier::CrushedDecal => {}
        }
    }

    // Distance-prioritized mass-conserving absorption into `PileField` when sleeping rubble exceeds soft cap (`Issue #16`)
    if sleeping_count > SOFT_DEBRIS_CLEANUP_THRESHOLD
        && let Some(pf) = pile_field.as_deref_mut()
    {
        // Farthest first; equal priorities fall back to the stable SimOrdinal, not query order.
        cleanup_candidates.sort_by(|a, b| {
            b.4.partial_cmp(&a.4)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.5.cmp(&b.5))
        });
        let to_absorb = sleeping_count
            .saturating_sub(SOFT_DEBRIS_CLEANUP_THRESHOLD)
            .min(cleanup_candidates.len());
        for i in 0..to_absorb {
            if let Some(&(ent, pos, mass, mat, _, _)) = cleanup_candidates.get(i) {
                pf.add_mass(pos, mass, mat);
                debris_pool.active_ring.retain(|&e| e != ent);
                if let Some(e_cmds) = commands.get_entity(ent) {
                    e_cmds.despawn_recursive();
                }
                sleeping_count = sleeping_count.saturating_sub(1);
            }
        }
    }

    let mut intact_nodes: usize = 0;
    let mut in_flight: usize = 0;
    for entry in graph_registry.buildings.values() {
        if entry.in_flight_task.is_some() {
            in_flight = in_flight.saturating_add(1);
        }
        intact_nodes =
            intact_nodes.saturating_add(entry.nodes.values().filter(|n| n.intact).count());
    }

    telemetry.active_physics_debris = active_count;
    telemetry.sleeping_rubble_count = sleeping_count;
    telemetry.active_bfs_tasks = in_flight;
    telemetry.intact_structural_nodes = intact_nodes;
}

/// Spawns initial modular buildings, interior flooring, interior furniture, and exterior street
/// props in Cartesian `WorldPose` space with 2.5D multi-box assemblies and per-cell compound
/// colliders (`Issues #7, #10, #14, #15, and #17`).
pub fn ensure_site_buildings_spawned_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    env_config: Res<EnvironmentConfig>,
    manifest: Res<CampaignPrefabsManifest>,
    _atlas: Res<ProceduralWorldAtlas>,
    mut graph_registry: ResMut<BuildingGraphRegistry>,
    test_map: Option<Res<SiteTestMap>>,
    existing_buildings: Query<Entity, With<ModularBuilding>>,
) {
    if existing_buildings.iter().next().is_some() {
        return;
    }

    graph_registry.current_epoch = *epoch;
    if test_map.is_some_and(|m| *m == SiteTestMap::Ranch) {
        spawn_ranch_benchmark_site(&mut commands, *epoch, &mut graph_registry);
        return;
    }
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
        let floor_z = iso_depth_key(offset, 0.5, ISO_BIAS_INTERIOR_FLOOR);
        commands.spawn((
            Name::new(format!("Building #{idx} Interior Flooring")),
            SiteScoped { epoch: *epoch },
            WorldPose::new(offset, 0.5, 0.0),
            IsoLayerBias(ISO_BIAS_INTERIOR_FLOOR),
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
            let prop_z = iso_depth_key(prop_pos, 2.0, ISO_BIAS_PROP);
            let (prop_base, prop_asm) = build_prop_iso_assembly(prop_arch);

            commands.spawn((
                Name::new(format!("Interior Prop ({prop_arch:?})")),
                SiteScoped { epoch: *epoch },
                SimOrdinal(SIM_ORDINAL_PROP_BASE.saturating_add((idx * 4 + p_i) as u64)),
                DestructibleProp::new(prop_arch, true),
                WorldPose::new(prop_pos, 2.0, 0.0),
                IsoLayerBias(ISO_BIAS_PROP),
                prop_base,
                prop_asm,
                Transform::from_xyz(prop_iso.x, prop_iso.y, prop_z),
                GlobalTransform::default(),
                Visibility::default(),
            ));
        }

        let entry = BuildingGraphEntry::new(*epoch, nodes)
            .with_site_ordinal(u32::try_from(idx).unwrap_or(u32::MAX));
        let (b_base, b_asm) = build_building_iso_assembly_from_entry(&entry, stories, idx);
        let compound_collider = build_building_compound_collider(&entry.nodes)
            .unwrap_or_else(|| Collider::rectangle(108.0, 76.0));
        let b_iso = world_to_iso_screen(offset, 0.0);
        let b_z = iso_depth_key(offset, f32::from(stories) * 16.0, ISO_BIAS_BUILDING);

        let b_entity = commands
            .spawn((
                Name::new(format!("Modular Building #{idx}: {archetype_name} (L{level_idx})")),
                SiteScoped { epoch: *epoch },
                SimOrdinal(idx as u64),
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
                IsoLayerBias(ISO_BIAS_BUILDING),
                b_base,
                b_asm,
                Transform::from_xyz(b_iso.x, b_iso.y, b_z),
                GlobalTransform::default(),
                Visibility::default(),
                (
                    Position(offset),
                    Rotation::IDENTITY,
                    RigidBody::Static,
                    compound_collider,
                ),
            ))
            .id();

        graph_registry.buildings.insert(b_entity, entry);
    }

    // Spawn exterior street props covering all 7 destruction profiles
    let exterior_specs = [
        (PropArchetype::PropaneTank, Vec2::new(-230.0, 90.0)),
        (PropArchetype::FireHydrant, Vec2::new(-90.0, 40.0)),
        (PropArchetype::Dumpster, Vec2::new(230.0, 95.0)),
        (PropArchetype::Mailbox, Vec2::new(90.0, 40.0)),
        (PropArchetype::PropaneTank, Vec2::new(75.0, 190.0)),
        (PropArchetype::ConcreteBarrier, Vec2::new(-75.0, 190.0)),
        (PropArchetype::UtilityPole, Vec2::new(-210.0, 40.0)),
        (PropArchetype::PineTree, Vec2::new(210.0, 40.0)),
        (PropArchetype::HayBale, Vec2::new(-130.0, 195.0)),
    ];

    for (ext_idx, (arch, pos)) in exterior_specs.into_iter().enumerate() {
        let prop_iso = world_to_iso_screen(pos, 0.0);
        let prop_z = iso_depth_key(pos, 6.0, ISO_BIAS_PROP);
        let (prop_base, prop_asm) = build_prop_iso_assembly(arch);
        commands.spawn((
            Name::new(format!("Exterior Street Prop ({arch:?})")),
            SiteScoped { epoch: *epoch },
            SimOrdinal(SIM_ORDINAL_PROP_BASE.saturating_add(500 + ext_idx as u64)),
            DestructibleProp::new(arch, false),
            WorldPose::new(pos, 0.0, 0.0),
            IsoLayerBias(ISO_BIAS_PROP),
            prop_base,
            prop_asm,
            Transform::from_xyz(prop_iso.x, prop_iso.y, prop_z),
            GlobalTransform::default(),
            Visibility::default(),
        ));
    }
}

/// Ranch benchmark ([`SiteTestMap::Ranch`]), hand-built from the reference's
/// `residential/ranch/main.building.json`: 5x3 cells, one floor, timber walls and posts, gable
/// roof along x (three West/Center/East bays), with kitchen / living / bath fixtures inside.
/// The reference's chimney and south door opening are not modeled (no such modules yet).
pub fn spawn_ranch_benchmark_site(
    commands: &mut Commands,
    epoch: SessionEpoch,
    graph_registry: &mut BuildingGraphRegistry,
) {
    let origin = RANCH_SITE_ORIGIN;
    let mut nodes = build_rect_building_graph(1, 2, 1);
    for node in &mut nodes {
        if node.kind == StructuralModuleKind::LoadBearingPillar {
            // Timber house: corner posts are wood, not the procedural brick piers.
            node.material = MaterialKind::Wood;
            node.facade_material = MaterialKind::Wood;
            node.hp = MaterialKind::Wood.default_hp();
            node.max_hp = node.hp;
        }
    }

    let floor_iso = world_to_iso_screen(origin, 0.5);
    commands.spawn((
        Name::new("Ranch Interior Flooring"),
        SiteScoped { epoch },
        WorldPose::new(origin, 0.5, 0.0),
        IsoLayerBias(ISO_BIAS_INTERIOR_FLOOR),
        IsoExtrudedBox::new(104.0, 44.0, 1.8, Color::srgb(0.80, 0.70, 0.55)).with_shadow(false),
        Transform::from_xyz(floor_iso.x, floor_iso.y, iso_depth_key(origin, 0.5, ISO_BIAS_INTERIOR_FLOOR)),
        GlobalTransform::default(),
        Visibility::default(),
    ));

    // Kitchen (west), living (center), bath (east), as in main.layout.json.
    let fixtures = [
        (PropArchetype::Refrigerator, Vec2::new(-38.0, 6.0)),
        (PropArchetype::Couch, Vec2::new(0.0, 8.0)),
        (PropArchetype::Toilet, Vec2::new(38.0, 4.0)),
    ];
    for (i, (arch, local)) in fixtures.into_iter().enumerate() {
        let pos = origin + local;
        let (prop_base, prop_asm) = build_prop_iso_assembly(arch);
        let iso = world_to_iso_screen(pos, 2.0);
        commands.spawn((
            Name::new(format!("Ranch Fixture ({arch:?})")),
            SiteScoped { epoch },
            SimOrdinal(SIM_ORDINAL_PROP_BASE.saturating_add(i as u64)),
            DestructibleProp::new(arch, true),
            WorldPose::new(pos, 2.0, 0.0),
            IsoLayerBias(ISO_BIAS_PROP),
            prop_base,
            prop_asm,
            Transform::from_xyz(iso.x, iso.y, iso_depth_key(pos, 2.0, ISO_BIAS_PROP)),
            GlobalTransform::default(),
            Visibility::default(),
        ));
    }

    let entry = BuildingGraphEntry::new(epoch, nodes).with_site_ordinal(0);
    let (b_base, b_asm) = build_building_iso_assembly_from_entry(&entry, 1, 0);
    let collider = build_building_compound_collider(&entry.nodes)
        .unwrap_or_else(|| Collider::rectangle(108.0, 46.0));
    let b_iso = world_to_iso_screen(origin, 0.0);
    let b_entity = commands
        .spawn((
            Name::new("Ranch (benchmark)"),
            SiteScoped { epoch },
            SimOrdinal(0),
            ModularBuilding {
                archetype_name: "RANCH".to_string(),
                stories: 1,
                palette_index: 0,
            },
            CutawayFootprint {
                min_world: origin - Vec2::new(60.0, 32.0),
                max_world: origin + Vec2::new(60.0, 32.0),
                exterior_breached: false,
                current_roof_alpha: 1.0,
            },
            WorldPose::new(origin, 0.0, 0.0),
            IsoLayerBias(ISO_BIAS_BUILDING),
            b_base,
            b_asm,
            Transform::from_xyz(b_iso.x, b_iso.y, iso_depth_key(origin, 16.0, ISO_BIAS_BUILDING)),
            GlobalTransform::default(),
            Visibility::default(),
            (Position(origin), Rotation::IDENTITY, RigidBody::Static, collider),
        ))
        .id();
    graph_registry.buildings.insert(b_entity, entry);
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
            .init_resource::<CollapseTaskJoinStats>()
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
                    sync_graph_registry_clock_system.in_set(PdSystemSet::SimInput),
                    (
                        poll_structural_collapse_tasks_system,
                        step_building_roofs_and_collapse_sites_system,
                    )
                        .chain()
                        .in_set(PdSystemSet::DestructionGraph),
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

        let (c_min, c_max) = StructuralNode::cell_bounds_for_grid(0, 0);
        // 1. Disconnected single-node upper story floating at story_level = 2
        let floating_node = StructuralNode {
            id: 0,
            kind: StructuralModuleKind::RoofCorrugation,
            material: MaterialKind::Wood,
            facade_material: MaterialKind::Wood,
            hp: 26.0,
            max_hp: 26.0,
            cladding_hp: 0.0,
            cell_min: c_min,
            cell_max: c_max,
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

    #[test]
    fn test_per_cell_breaching_opens_south_wall_doorway_while_other_walls_block() {
        let nodes = build_procedural_building_graph(1, 2);
        let mut entry = BuildingGraphEntry::new(SessionEpoch(1), nodes);
        let b_origin = Vec2::new(0.0, 80.0);

        // Initially, south wall cell at (0, -2) (world center (0.0, 50.0)) blocks a circle of radius 10.0
        let south_door_world = Vec2::new(0.0, 50.0);
        let north_wall_world = Vec2::new(0.0, 110.0);
        assert!(building_blocks_circle(&entry, b_origin, south_door_world, 10.0));
        assert!(building_blocks_circle(&entry, b_origin, north_wall_world, 10.0));

        // Merged spans initially have 1 contiguous south wall span of 3 Wall cells (between the 2 corner pillars)
        let south_spans_before = entry
            .cached_wall_spans
            .iter()
            .filter(|s| s.orientation == WallSpanOrientation::HorizontalX && s.local_center.y < 0.0)
            .count();
        assert_eq!(south_spans_before, 1);

        // Apply partial damage to center south wall cell (0, -2) -> transitions to CrackedStressed
        let center_south_id = entry
            .nodes
            .values()
            .find(|n| n.story_level == 0 && n.grid_x == 0 && n.grid_y == -2)
            .map(|n| n.id)
            .unwrap_or(0);
        if let Some(cell) = entry.nodes.get_mut(&center_south_id) {
            let breached = cell.apply_localized_damage(cell.max_hp * 0.50);
            assert!(!breached);
            assert_eq!(cell.damage_state, DamageState::CrackedStressed);
        }

        // Now breach the south wall doorway cells (-1, -2), (0, -2), (1, -2)
        for gx in -1..=1 {
            if let Some(cell) = entry
                .nodes
                .values_mut()
                .find(|n| n.story_level == 0 && n.grid_x == gx && n.grid_y == -2)
            {
                let breached = cell.apply_localized_damage(999.0);
                assert!(breached);
                assert!(!cell.intact);
            }
        }
        entry.visual_revision = entry.visual_revision.saturating_add(1);
        entry.refresh_surface_cache();

        // South doorway (0, 50) and interior room (0, 80) are now open and passable, while north wall (0, 110) still blocks!
        assert!(!building_blocks_circle(&entry, b_origin, south_door_world, 6.0));
        assert!(!building_blocks_circle(&entry, b_origin, b_origin, 10.0));
        assert!(building_blocks_circle(&entry, b_origin, north_wall_world, 10.0));
    }

    #[test]
    fn test_merged_wall_spans_split_on_single_cell_crack_or_breach() {
        let nodes = build_procedural_building_graph(1, 2);
        let mut entry = BuildingGraphEntry::new(SessionEpoch(1), nodes);

        // South facade at gy == -2 has 3 interior Wall cells (gx = -1, 0, 1) merged into 1 WallSpan
        let initial_south: Vec<&WallSpan> = entry
            .cached_wall_spans
            .iter()
            .filter(|s| s.orientation == WallSpanOrientation::HorizontalX && s.local_center.y < 0.0)
            .collect();
        assert_eq!(initial_south.len(), 1);
        assert_eq!(initial_south.first().map_or(0, |s| s.cell_count), 3);

        // Breach the middle cell (gx = 0, gy = -2): the single 3-cell span splits into two 1-cell spans!
        if let Some(mid) = entry
            .nodes
            .values_mut()
            .find(|n| n.story_level == 0 && n.grid_x == 0 && n.grid_y == -2)
        {
            mid.apply_localized_damage(999.0);
        }
        entry.visual_revision = entry.visual_revision.saturating_add(1);
        entry.refresh_surface_cache();

        let split_south: Vec<&WallSpan> = entry
            .cached_wall_spans
            .iter()
            .filter(|s| s.orientation == WallSpanOrientation::HorizontalX && s.local_center.y < 0.0)
            .collect();
        assert_eq!(split_south.len(), 2);
    }

    #[test]
    fn test_debris_chunk_damps_horizontal_velocity_before_settling() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<RoadSplineGraph>()
            .init_resource::<DebrisPoolRegistry>()
            .init_resource::<BuildingGraphRegistry>()
            .init_resource::<DebugTelemetry>()
            .add_systems(Update, update_debris_lifecycle_system);

        let entity = app
            .world_mut()
            .spawn((
                DebrisTier::ActivePhysics,
                DebrisChunk {
                    tier: DebrisTier::ActivePhysics,
                    active_timer_secs: 4.0,
                    simulated_z_height: 0.0,
                    vertical_z_velocity: 0.0,
                    horizontal_velocity: Vec2::new(24.0, 0.0),
                    spin_rate: 2.2,
                    ..Default::default()
                },
                WorldPose::new(Vec2::ZERO, 0.0, 0.0),
                Position(Vec2::ZERO),
                Rotation::IDENTITY,
                Transform::default(),
                LinearVelocity::default(),
            ))
            .id();

        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(16));
        app.update();

        // After a single ground-level tick, horizontal_velocity is still well above
        // the settlement threshold (speed_sq < 4.0), even though LinearVelocity is zero.
        let chunk_after_1 = app.world().get::<DebrisChunk>(entity);
        assert!(chunk_after_1.is_some());
        if let Some(chunk) = chunk_after_1 {
            assert_eq!(chunk.tier, DebrisTier::ActivePhysics);
            assert!(chunk.horizontal_velocity.x > 16.0 && chunk.horizontal_velocity.x < 24.0);
        }

        // Advance enough ticks for ground damping to decay horizontal_velocity below 2.0 (speed_sq < 4.0)
        for _ in 0..45 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_millis(16));
            app.update();
        }

        let chunk_settled = app.world().get::<DebrisChunk>(entity);
        assert!(chunk_settled.is_some());
        if let Some(chunk) = chunk_settled {
            assert_eq!(chunk.tier, DebrisTier::SleepingInstanced);
        }
    }
}
