---
name: industrial-hud-state
description: >-
  Defensive invariants for pd_ui and the pd_core economy in Permit Denied:
  top/bottom industrial HUD bars, the 6-digit mechanical cash odometer,
  ENGINE HEAT / TRACK STRESS dial needles, floating cash popup coalescing
  (try_award_demolition_cash), and modal/state transitions (Title, Pause,
  Restart Site, New Game) with idempotent site restart. Use when working on
  pd_ui, HUD widgets, modals, CashLedger, CashPopupPool, or restart/new-game
  flows.
---

# Industrial HUD & State

Use this playbook when implementing or modifying `pd_ui` and the `pd_core` economy/HUD contracts.

## 1. Core defensive invariants

1. **Contract: `try_award_demolition_cash`**
   - Signature: `pub fn try_award_demolition_cash(ledger: &mut CashLedger, popup_pool: &mut CashPopupPool, amount: u32, world_pos: Vec3, current_time_secs: f32, fallback_pos: Vec3) -> Result<u32, LedgerError>`
   - If `amount == 0`, return `Ok(ledger.current_cash)` immediately.
   - Use `saturating_add` and clamp to `MAX_ODOMETER_CASH` (`999_999`) so the 6-digit odometer (`$000,000`–`$999,999`) never overflows.
   - Validate `world_pos.is_finite()`; if not finite, substitute `fallback_pos` (or `Vec3::ZERO` if `fallback_pos` is also non-finite).
   - **Spatial/temporal coalescing:** if an active popup is within `COALESCE_RADIUS` (`3.0m`) and was spawned within `COALESCE_WINDOW_SECS` (`0.10s`), or the pool is at capacity (`50` active slots), coalesce `amount` into the nearest active popup using `saturating_add`.
2. **Idempotent site restart (`Restart Site` / `New Game`):**
   - Increment `SessionEpoch`.
   - Reset `CashLedger`, the elapsed timer, `MechanicalStress`, `UpgradeMultipliers` / `UpgradeProgression` (restore `level_entry_upgrades` on retry, snapshot installed upgrades on level advance, or reset to `1.0x` on `New Game`), and `CashPopupPool`.
   - Despawn all site-scoped entities cleanly without leaking UI nodes or audio handles.

## 2. Verification gate

- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- `test_kinematic_and_ledger_extremes` and `test_idempotent_site_restart` pass in `cargo test --workspace`.
