---
name: rust-defensive-ecs
description: >-
  Core Bevy plugin scaffolding, SystemSet architecture, custom error types,
  SessionEpoch validation, and non-panicking query patterns. Use this skill when
  creating or modifying Bevy plugins, ECS systems, resources, async tasks, or
  state transitions across any crate in Permit Denied.
---

# Rust Defensive ECS Skill (`rust-defensive-ecs`)

Follow these defensive Bevy ECS patterns whenever writing systems, resources, or plugins in `permit-denied-bevy`.

## 1. Non-Panicking Queries & Safe Option/Result Handling

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

## 2. Float Sanitization & Delta-Time Clamping

All `FixedUpdate` physics and kinematic systems must clamp `dt` to `[0.0001, 0.05]` and verify all computed `f32`, `Vec2`, and `Vec3` values are `.is_finite()` before writing to `Transform` or velocity components.

```rust
pub fn clamp_dt(raw_dt: f32) -> Result<f32, PhysicsError> {
    if !raw_dt.is_finite() || raw_dt <= 0.0 {
        return Err(PhysicsError::InvalidDeltaTime(raw_dt));
    }
    Ok(raw_dt.clamp(0.0001, 0.05))
}
```

## 3. `SessionEpoch` Guard for Async Tasks & Deferred Commands

Every world reset (`Restart Site`, `New Game`, `Main Menu`) increments `SessionEpoch(pub u64)`. Any async task or deferred event must stamp the `SessionEpoch` when queued and verify it matches before mutating the world.

## 4. Verification Gate

Run all three gates before completing your task:
1. `cargo clippy --workspace --all-targets -- -D warnings`
2. `cargo test --workspace` (including `test_kinematic_and_ledger_extremes` and `test_idempotent_site_restart`)
