---
name: crowd-traffic-spatial-ai
description: >-
  Spatial hash grid partitioning, pedestrian 8-directional panic/flee boid
  steering, and spline-to-physics vehicle transitions. Use this skill when
  working on pd_ai, pedestrian crowds, or traffic vehicles.
---

# Crowd & Traffic Spatial AI Skill (`crowd-traffic-spatial-ai`)

Use this playbook when implementing or modifying `pd_ai`.

## 1. Core Defensive Invariants

1. **Contract: `update_crowd_and_traffic_spatial_grid`**
   * Query `SpatialHashGrid` only within `THREAT_RADIUS` of the dozer and active `CollapseZone` entries—never iterate over all map entities in nested $O(N^2)$ loops.
2. **Zero-Distance Escape Guard (Division-by-Zero Protection):**
   * When computing the flee vector `agent_pos - threat_pos`, if `length_squared() < 1e-6`, use a deterministic fallback unit vector (`Vec2::X` rotated by entity index hash) rather than calling `.normalize()` and producing `NaN`.
3. **Iteration-Capped Local Avoidance (Max 8 Neighbors):**
   * Fleeing pedestrians use lightweight boid/vector steering with a hard ceiling of `MAX_NEIGHBOR_CHECKS = 8` per agent per tick (zero A* pathfinding during panic).
4. **Spline-to-Physics Vehicle Transition:**
   * Vehicles follow road lane splines until struck by the dozer or falling debris, or blocked by rubble (`BlockedByRubble` halts/reverses cleanly).
   * On impact, strip `SplineFollower` and insert dynamic physics velocity clamped to `MAX_VEHICLE_KNOCKBACK_SPEED`.

## 2. Verification Gate

* Run `cargo clippy --workspace --all-targets -- -D warnings`.
* Verify zero `NaN` velocities when threat position coincides exactly with pedestrian position (`distance == 0.0`).
