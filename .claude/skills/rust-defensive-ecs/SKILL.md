---
name: rust-defensive-ecs
description: >-
  Core defensive Bevy patterns for Permit Denied: plugin scaffolding,
  SystemSet architecture, custom error types, non-panicking queries and
  Option/Result handling, dt clamping and float sanitization, and SessionEpoch
  validation for async tasks and deferred commands. Use when creating or
  modifying any Bevy plugin, ECS system, resource, async task, or state
  transition in any crate.
---

# Rust Defensive ECS

Follow these defensive Bevy ECS patterns whenever writing systems, resources or plugins in `permit-denied-bevy`.

## 1. Non-panicking queries & safe Option/Result handling

Never call `.unwrap()`, `.expect()`, `.single()`, `.single_mut()`, or slice indexing `slice[i]` in runtime code.

```rust
// GOOD: Non-panicking query and safe slice access
pub fn update_player_system(
    mut query: Query<(&mut Transform, &PlayerVehicle)>,
) {
    let Ok((mut transform, vehicle)) = query.get_single_mut() else {
        return;
    };
    if let Some(first_waypoint) = vehicle.waypoints.first() {
        // Safe access
    }
}
```

## 2. Float sanitization & delta-time clamping

All `FixedUpdate` physics and kinematic systems clamp `dt` to `[0.0001, 0.05]` and verify every computed `f32`, `Vec2` and `Vec3` is `.is_finite()` before writing to `Transform` or velocity components.

```rust
pub fn clamp_dt(raw_dt: f32) -> Result<f32, PhysicsError> {
    if !raw_dt.is_finite() || raw_dt <= 0.0 {
        return Err(PhysicsError::InvalidDeltaTime(raw_dt));
    }
    Ok(raw_dt.clamp(0.0001, 0.05))
}
```

## 3. `SessionEpoch` guard for async tasks & deferred commands

Every world reset (`Restart Site`, `New Game`, `Main Menu`) increments `SessionEpoch(pub u64)`. Any async task or deferred event stamps the `SessionEpoch` when queued and verifies it still matches — and that the target entity still exists — before mutating the world.

## 4. Deterministic simulation (record / replay)

Same seed + same tick-indexed inputs must reproduce a run bit for bit (CLAUDE.md Rule B):

- Sim-state systems go in a `FixedUpdate` `PdSystemSet` (gated by `pd_core::sim_running`). `Update` and render systems never write sim state.
- Randomness only from `SiteSeed` + `SplitMix64`. No wall clock, no `Entity::index()`, no `HashMap` or query-order dependence where order changes the result; sort by `SimOrdinal` (`fill_sim_order`) into a `Local` buffer.
- Async results join on a fixed tick. Stop the sim with `SimHold` (gameplay) or `SimFreeze` (tooling), never by leaving `Playing` mid-frame.
- Prove it: `tests/determinism.rs` and the replay MATCH tests (Gate 4).

## 5. Verification gate

Pass all four gates (`CLAUDE.md` Rule Group E) before completing the task:

1. `cargo clippy --workspace --all-targets -- -D warnings` (Gate 1)
2. `cargo test --workspace`, including `test_kinematic_and_ledger_extremes` (Gate 2), `test_idempotent_site_restart` (Gate 3) and the determinism / replay MATCH tests (Gate 4)
