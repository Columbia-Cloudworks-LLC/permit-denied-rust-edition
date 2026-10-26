---
name: crowd-traffic-spatial-ai
description: >-
  Defensive invariants for pd_ai in Permit Denied: spatial hash grid
  partitioning, pedestrian 8-directional panic/flee boid steering with
  zero-distance guards, and spline-to-physics vehicle transitions. Use when
  working on pd_ai, pedestrian crowds, panic/flee behavior, SpatialHashGrid,
  or traffic vehicles.
---

# Crowd & Traffic Spatial AI

Use this playbook when implementing or modifying `pd_ai`.

## 1. Core defensive invariants

1. **Contract: `update_crowd_and_traffic_spatial_grid`**
   - Query `SpatialHashGrid` only within `THREAT_RADIUS` of the dozer and active `CollapseZone` entries — never iterate over all map entities in nested O(N²) loops.
2. **Zero-distance escape guard (division-by-zero protection):**
   - When computing the flee vector `agent_pos - threat_pos`, if `length_squared() < 1e-6`, use a deterministic fallback unit vector (`Vec2::X` rotated by an entity-index hash) instead of calling `.normalize()` and producing `NaN`.
3. **Iteration-capped local avoidance (max 8 neighbors):**
   - Fleeing pedestrians use lightweight boid/vector steering with a hard ceiling of `MAX_NEIGHBOR_CHECKS = 8` per agent per tick (no A* pathfinding during panic).
4. **Spline-to-physics vehicle transition:**
   - Vehicles follow road lane splines until struck by the dozer or falling debris, or blocked by rubble (`BlockedByRubble` halts/reverses cleanly).
   - On impact, strip `SplineFollower` and insert dynamic physics velocity clamped to `MAX_VEHICLE_KNOCKBACK_SPEED`.

## 2. Verification gate

- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- A test verifies zero `NaN` velocities when the threat position coincides exactly with a pedestrian position (`distance == 0.0`).
