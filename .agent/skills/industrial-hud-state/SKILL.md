---
name: industrial-hud-state
description: >-
  Top/Bottom industrial HUD bars, 6-digit mechanical odometer reel interpolation,
  analog dial needles (ENGINE HEAT / TRACK STRESS), floating cash popup coalescing,
  and modal state transitions (Title, Pause, Restart Site, New Game). Use this
  skill when working on pd_ui or pd_core ledger/popup systems.
---

# Industrial HUD & State Skill (`industrial-hud-state`)

Use this playbook when implementing or modifying `pd_ui` and `pd_core` economy/HUD contracts.

## 1. Core Defensive Invariants

1. **Contract: `try_award_demolition_cash`**
   * Signature: `pub fn try_award_demolition_cash(ledger: &mut CashLedger, popup_pool: &mut CashPopupPool, amount: u32, world_pos: Vec3, current_time_secs: f32, fallback_pos: Vec3) -> Result<u32, LedgerError>`
   * If `amount == 0`, return `Ok(ledger.current_cash)` immediately.
   * Use `saturating_add` and clamp to `MAX_ODOMETER_CASH` (`999_999`) so the 6-digit mechanical odometer (`$000,000`–`$999,999`) never overflows.
   * Validate `world_pos.is_finite()`; if not finite, substitute `fallback_pos` (or `Vec3::ZERO` if `fallback_pos` is also non-finite).
   * **Spatial/Temporal Coalescing:** If an active popup in `popup_pool` is within `COALESCE_RADIUS` (`3.0m`) and spawned within `COALESCE_WINDOW_SECS` (`0.10s`), or if `popup_pool` is at capacity (`50` active slots), coalesce `amount` into the nearest active popup using `saturating_add`.
2. **Idempotent Site Restart (`Restart Site` / `New Game`):**
   * Increment `SessionEpoch`.
   * Reset `CashLedger`, elapsed timer, `MechanicalStress`, `UpgradeMultipliers`, and `CashPopupPool`.
   * Despawn all site-scoped entities cleanly without leaking UI nodes or audio handles.

## 2. Verification Gate

* Run `cargo clippy --workspace --all-targets -- -D warnings`.
* Ensure `test_kinematic_and_ledger_extremes` and `test_idempotent_site_restart` pass in `cargo test --workspace`.
