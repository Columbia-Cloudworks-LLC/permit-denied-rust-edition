---
name: dozer-kinematics-physics
description: >-
  Dual-track differential steering, BLADE UP / BLADE DOWN state machine,
  AR400 Edge / Charge-Air Kit / Aux Ram upgrade multipliers, ENGINE HEAT /
  TRACK STRESS gauges, and anti-wedging depenetration watchdog. Use this skill
  when working on pd_dozer.
---

# Dozer Kinematics & Physics Skill (`dozer-kinematics-physics`)

Use this playbook when implementing or modifying `pd_dozer`.

## 1. Core Defensive Invariants

1. **Contract: `compute_skid_steer_kinematics`**
   * Signature: `pub fn compute_skid_steer_kinematics(input: &TrackInput, blade_state: BladeState, upgrades: &UpgradeMultipliers, stress: &mut MechanicalStress, dt: f32) -> Result<KinematicOutput, PhysicsError>`
   * Validate `dt` is finite and positive; clamp to `[0.0001, 0.05]`.
   * Clamp upgrade multipliers (`ar400_edge`, `charge_air_kit`, `aux_ram`) to `[1.0, MAX_SAFE_MULTIPLIER]` (`25.0`) so corrupted state cannot produce infinite velocity.
   * Pressing or holding `Space` / `F` / `R` / `Left Shift` lowers the blade (`BladeState::BladeDown`), triggering a timed powered push surge (`DOZER_PUSH_SECONDS * aux_ram`, up to `1.12x` top speed followed by `DOZER_PUSH_COOLDOWN`). While held down outside an active forward push, apply `0.74x` (`DOZER_BLADE_DOWN_SPEED_DRAG`) travel drag, multiply structural shear damage by `ar400_edge`, and increase heat/stress accumulation under load.
   * When `stress.sandbox_mode` is `true` (`GameMode::Sandbox`), clamp `stress.engine_heat_pct` and `stress.track_stress_pct` below breakdown thresholds (`75.0%` max, Sandbox breakdown immunity).
   * Verify linear and angular velocity outputs are `.is_finite()` before returning `Ok(KinematicOutput)`.
2. **Interactive Milestone & Sandbox Upgrades (`UpgradeProgression`):**
   * In `GameMode::Campaign`, crossing cash milestones (`$350`, `$900`, `$1,600`, `+$900`...) increments `pending_upgrade_choices` and pauses the simulation until the player selects `1` (`AR400 Edge +42%`), `2` (`Charge-Air Kit +28%`), or `3` (`Aux Ram +35%`). Advancing levels snapshots `level_entry_upgrades`, while retrying a site restores `level_entry_upgrades`.
   * In `GameMode::Sandbox`, pressing `1` / `2` / `3` applies upgrades immediately without pausing or requiring cash milestones.
3. **Anti-Wedging & Depenetration Watchdog:**
   * Cap maximum impulse applied to the dozer per `FixedUpdate` tick (`MAX_DOZER_IMPULSE_PER_TICK`).
   * Track consecutive ticks where penetration depth exceeds `MAX_SAFE_PENETRATION_DEPTH`. If `> 3` consecutive ticks, crush the intersecting block or displace the dozer `+0.5m` along its safe recovery normal.

## 2. Verification Gate

* Run `cargo clippy --workspace --all-targets -- -D warnings`.
* Run `cargo test -p pd_dozer` including `test_kinematic_and_ledger_extremes` (`dt = 0.0`, `dt = 999.0`, `NaN` track inputs, extreme multipliers).
