---
name: destruction-graph-pooling
description: >-
  Modular building node graphs, async BFS connectivity evaluation, 3-tier debris
  lifecycle, simulated 2.5D Z-height collapse forces, and interior furniture
  spawning. Use this skill when working on pd_destruction, building prefabs,
  structural collapse, or debris pooling.
---

# Destruction Graph & Debris Pooling Skill (`destruction-graph-pooling`)

Use this playbook when implementing or updating `pd_destruction`.

## 1. Core Defensive Invariants

1. **Contract: `queue_structural_integrity_evaluation`**
   * Signature: `pub fn queue_structural_integrity_evaluation(building_id: Entity, destroyed_nodes: &[NodeIndex], graph_registry: &mut BuildingGraphRegistry, current_epoch: SessionEpoch) -> EvalStatus`
   * If `building_id` is missing or already `Collapsing`, return `EvalStatus::Skipped`.
   * Remove `destroyed_nodes` using safe `.get_mut()` lookups and increment `dirty_generation` via `saturating_add(1)`.
   * Coalesce concurrent evaluations on the same `building_id` instead of spawning duplicate async tasks.
   * Cap BFS traversal iterations strictly at `MAX_GRAPH_ITERATIONS` (`4096`) to guarantee termination even on cyclic or corrupted `.ron` graphs.
2. **Epoch & Generation Validation:**
   * When an async BFS collapse evaluation completes, verify both `result.epoch == current_epoch` and `building.dirty_generation == result.generation` before spawning falling debris clusters.
3. **3-Tier Debris Ring-Buffer Pool (Max 500 Active Physics Bodies):**
   * Tier 1 (*Active Physics Debris*): `avian2d` rigid bodies active for 3–5 seconds with simulated vertical Z-drop bounce.
   * Tier 2 (*Sleeping Instanced Props*): Once velocity settles below threshold or timer expires, strip physics colliders and convert to static rubble sprites.
   * Tier 3 (*Heightmap Crushing*): Driving over sleeping rubble with `BladeState::BladeDown` flattens the sprite into the persistent ground decal layer.
   * When spawning at the 500-body ceiling, `recycle_oldest_debris` deterministically evicts the oldest active body into Tier 2 and reuses its slot.

## 2. Verification Gate

* Run `cargo clippy --workspace --all-targets -- -D warnings`.
* Ensure `test_kinematic_and_ledger_extremes` (disconnected single-node / cyclic graphs) and `test_idempotent_site_restart` pass in `cargo test --workspace`.
