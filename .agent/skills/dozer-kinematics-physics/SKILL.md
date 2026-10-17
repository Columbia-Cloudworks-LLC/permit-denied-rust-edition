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
   * Signature: `pub fn compute_skid_steer_kinematics(input: &TrackInput, blade_state: BladeState, upgrades: &UpgradeMultipliers, stress: &mut MechanicalStress, game_mode: GameMode, dt: f32) -> Result<KinematicOutput, PhysicsError>`
   * Validate `dt` is finite and positive; clamp to `[0.0001, 0.05]`.
   * Clamp upgrade multipliers (`ar400_edge`, `charge_air_kit`, `aux_ram`) to `[1.0, MAX_SAFE_MULTIPLIER]` (`10.0`) so corrupted state cannot produce infinite velocity.
   * When `blade_state == BladeState::BladeDown`, increase resistive drag, multiply structural shear damage by `ar400_edge`, and increase heat/stress accumulation under load.
   * When `game_mode == GameMode::Sandbox`, clamp `stress.engine_heat` and `stress.track_stress` below breakdown thresholds (Sandbox breakdown immunity).
   * Verify linear and angular velocity outputs are `.is_finite()` before returning `Ok(KinematicOutput)`.
2. **Anti-Wedging & Depenetration Watchdog:**
   * Cap maximum impulse applied to the dozer per `FixedUpdate` tick (`MAX_DOZER_IMPULSE_PER_TICK`).
   * Track consecutive ticks where penetration depth exceeds `MAX_SAFE_PENETRATION_DEPTH`. If `> 3` consecutive ticks, crush the intersecting block or displace the dozer `+0.5m` along its safe recovery normal.

## 2. Verification Gate

* Run `cargo clippy --workspace --all-targets -- -D warnings`.
* Run `cargo test -p pd_dozer` including `test_kinematic_and_ledger_extremes` (`dt = 0.0`, `dt = 999.0`, `NaN` track inputs, extreme multipliers).
