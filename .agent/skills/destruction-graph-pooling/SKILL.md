---
name: destruction-graph-pooling
description: >-
  Spatial per-cell modular building node graphs, merged exterior WallSpans,
  locally supported sagging roof bays (Gable, Shed, Flat), async BFS connectivity
  evaluation, 4-layer mass-conserving debris lifecycle with PileField and
  CollapsedSite scars, and 7 data-driven prop destruction profiles. Use this
  skill when working on pd_destruction, building prefabs, structural collapse,
  or debris pooling.
---

# Destruction Graph & Debris Pooling Skill (`destruction-graph-pooling`)

Use this playbook when implementing or updating `pd_destruction`.

## 1. Core Defensive Invariants

1. **Spatial Per-Cell Building Graphs & Compound Colliders (`Issue #14`):**
   * Each `StructuralNode` owns material-specific `hp`, `max_hp`, `cladding_hp`, and local footprint bounds `(cell_min, cell_max)`.
   * Ground-floor solid obstacles (`Wall` and `LoadBearingPillar` at `story_level == 0`) generate a compound Avian2D collider via `build_building_compound_collider` and `building_blocks_circle` so breaching individual wall cells opens a passable doorway while intact walls continue blocking the dozer.
2. **Merged Exterior Wall Spans & Sagging Roof Bays (`Issue #15`):**
   * `extract_merged_wall_spans` merges contiguous exterior wall cells sharing `(story_level, facade_material, damage_state)` into single `WallSpan` boxes, cached on `BuildingGraphEntry::visual_revision`.
   * Locally supported `RoofBay`s (`Gable`, `Shed`, `Flat`) transition `Intact -> Sagging { progress, hinge_dir } -> Collapsed` when support drops below 50%, applying domino lean damage to adjacent buildings on collapse.
3. **Contract: `queue_structural_integrity_evaluation` & Epoch Validation:**
   * Signature: `pub fn queue_structural_integrity_evaluation(building_id: Entity, destroyed_nodes: &[NodeIndex], graph_registry: &mut BuildingGraphRegistry, task_pool: &AsyncComputeTaskPool) -> EvalStatus`
   * If `building_id` is missing or already `Collapsing`, return `EvalStatus::Skipped`.
   * Remove `destroyed_nodes` using safe `.get_mut()` lookups and increment `dirty_generation` and `visual_revision` via `saturating_add(1)`.
   * Coalesce concurrent evaluations on the same `building_id` instead of spawning duplicate async tasks.
   * Cap BFS traversal iterations strictly at `MAX_GRAPH_ITERATIONS` (`4096`) and evaluate material-dependent horizontal `beam_span()` support (`Wood = 2`, `Brick = 2`, `Concrete = 3`, `Steel = 4`).
   * Verify both `result.epoch == current_epoch` and `building.dirty_generation == result.generation` before spawning falling debris clusters.
4. **4-Layer Mass-Conserving Debris Lifecycle & `PileField` (`Issue #16`):**
   * Layer 1 (*Active Physics Debris*): `avian2d` rigid bodies (max `500` active ceiling via `DebrisPoolRegistry::register_or_recycle_oldest`) with simulated 2.5D vertical Z-drop and material-dependent ground friction.
   * Layer 2 (*Sleeping Instanced Rubble*): Settled chunks strip physics colliders and rest at `pile_field.height_at(pos)`. Distant unattended chunks above `SOFT_DEBRIS_CLEANUP_THRESHOLD` (`360`) are absorbed into `PileField` while strictly conserving mass and never despawning chunks near or recently touched by the dozer.
   * Layer 3 (*Editable `PileField` Height & Resistance Grid*): Stores continuous ground rubble mass, resistance, track compaction, and blade heap pushing (`push_pile_with_blade`).
   * Layer 4 (*`CollapsedSite` Footprint Scars*): Buildings reaching `>= 90%` destroyed nodes transition into a passable `CollapsedSite` foundation scar and award `full_collapse_bonus` exactly once (`collapse_bonus_paid`).
5. **7 Data-Driven Destruction Profiles (`Issue #17`):**
   * `PropArchetype::catalog_def()` maps every interior/exterior prop to `AssetCatalogDef` and one of 7 `DestructionProfile`s: `Brittle`, `BendSnap`, `Crush`, `Topple`, `Roll`, `PanelCollapse`, and `Explosive { max_generation }` (bounded at `MAX_EXPLOSION_GENERATION = 1`).

## 2. Verification Gate

* Run `cargo clippy --workspace --all-targets -- -D warnings`.
* Ensure `test_kinematic_and_ledger_extremes`, `test_idempotent_site_restart`, and `test_epic_3_per_cell_breach_sagging_roofs_pile_field_and_7_profiles` pass in `cargo test --workspace`.
