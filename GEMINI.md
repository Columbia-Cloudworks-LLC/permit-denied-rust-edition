# Permit Denied — Global Workspace Rules & Defensive Architecture

This file acts as the constitutional law for the **Permit Denied** Rust / Bevy repository (`permit-denied-bevy`). Every change, crate, and system must adhere to these five operational boundaries.

---

## Rule Group A: Compiler & Linter Hardening (Zero-Panic Policy)

* **Forbidden Operations:** Workspace-level Cargo lints deny panicking patterns across all runtime crates (`pd_core`, `pd_dozer`, `pd_world`, `pd_destruction`, `pd_ai`, `pd_ui`) and the main binary:
  * `clippy::unwrap_used = "deny"`
  * `clippy::expect_used = "deny"`
  * `clippy::panic = "deny"`
  * `clippy::todo = "deny"`
  * `clippy::unimplemented = "deny"`
  * `clippy::indexing_slicing = "deny"`
  * `clippy::float_cmp = "deny"`
  *(Unit test modules marked `#[cfg(test)]` may explicitly `#![allow(...)]` or `#[allow(...)]` only where needed for test assertions, though non-panicking `Result` tests are preferred).*
* **Arithmetic Safety:** Always use `checked_*` or `saturating_*` arithmetic for cash ledger updates, structural graph node indices, pool indices, and grid coordinates.
* **Float Sanitization:** Every function computing physics impulses, differential steering torque, simulated Z-height collapse kinematics, or gauge needle rotations (`ENGINE HEAT`, `TRACK STRESS`) must pass through a float-validation guard (`is_finite()`) before writing to a Bevy component. If a `NaN` or `Infinity` is detected, log a structured warning (`bevy::log::warn!`) and fall back to zero velocity or the previous frame's valid state.

---

## Rule Group B: Bevy ECS & Scheduling Hygiene

* **Explicit System Ordering:** No two systems that mutate the same Component or Resource may run in the same schedule without an explicit `SystemSet` ordering constraint (`.before(...)`, `.after(...)`, or `.chain()`).
* **Deterministic Fixed Timestep (`FixedUpdate`):** All skid-steer kinematics (`avian2d`), hydraulic blade interactions (`BLADE UP` / `BLADE DOWN`), and structural collision checks must execute inside `FixedUpdate` with a clamped delta-time guard (`[0.0001, 0.05]`). Rendering, camera interpolation, tilemap animations, and HUD animations (`Cash` odometer reels, `Elapsed` timer) run in `Update`.
* **Safe Query Access:** Systems must use non-panicking query methods (`get`, `get_mut`, `get_single`, `get_single_mut`, `iter`, `iter_mut`) and handle `Err` / `None` via early return + structured telemetry logging—never `single()` or `single_mut()`.
* **Epoch-Guarded Async & Commands:** Whenever an `Entity` ID is passed into an `AsyncComputeTaskPool` task or deferred command (such as structural collapse BFS calculations), the payload must include the current `SessionEpoch(u64)` counter. Before applying deferred mutations back to the `World`, the receiving system must verify both that `SessionEpoch` matches and that the target `Entity` still exists (`commands.get_entity(entity)`), preventing race conditions when a player triggers `Restart Site` or `New Game`.

---

## Rule Group C: Memory Budgets & Object Pooling

* **Zero Per-Frame Heap Churn in Hot Loops:** Systems running every frame (`Update`) or fixed tick (`FixedUpdate`) must not allocate new `Vec` or `HashMap` instances on the heap in hot loops; use pre-allocated scratch buffers stored in Bevy `Local<T>` parameters or fixed-capacity resources.
* **Hard Entity Ceilings:** Dynamic entities (falling building chunks, shattered furniture, floating `+$X` popups, particle bursts) must be governed by strict ring-buffer pools:
  * Active physics debris ceiling: `500` dynamic bodies max (`recycle_oldest_debris` strips collider and bakes into static ground rubble layer).
  * Floating cash popup ceiling: `50` active popups max (spatial/temporal coalescing within `100ms` and `2.0m–3.0m` radius).

---

## Rule Group D: Crate Dependency Hierarchy (Strict DAG)

Circular dependencies are strictly forbidden. Crates must obey this Directed Acyclic Graph (DAG):

1. `pd_core` — Bottom layer: game states, `SessionEpoch`, `CashLedger`, `CashPopupPool`, `Season`/`Weather`/`CampaignLevel` configs, float/math guards, custom error enums. Depends on **nothing** internal.
2. `pd_world` & `pd_dozer` — Depend **only** on `pd_core`.
3. `pd_destruction` & `pd_ai` — Depend **only** on `pd_core` and `pd_world`.
4. `pd_ui` — Reads state from `pd_core` and `pd_dozer` via read-only queries/events.

---

## Rule Group E: Defensive Verification Gates

Before marking any milestone or subsystem task complete, all three verification gates must pass:

1. **Gate 1 (`verify_no_panic_paths`):** `cargo clippy --workspace --all-targets -- -D warnings` passes with zero warnings or errors under the workspace lint rules.
2. **Gate 2 (`test_kinematic_and_ledger_extremes`):** Unit tests feed extreme/corrupted inputs (`dt = 0.0`, `dt = 999.0`, `NaN`/`INFINITY` vectors, `u32::MAX` cash additions, disconnected single-node buildings) into core functions and assert clean `Err(...)` or clamped `Ok(...)` returns without panicking.
3. **Gate 3 (`test_idempotent_site_restart`):** Headless Bevy world test spawns a city block, triggers a building collapse, dispatches `Restart Site` mid-collapse, ticks 10 frames, and verifies that the `SessionEpoch` advances, stale async tasks are discarded, and zero orphaned site entities remain.

---

## Rule Group F: Pull Request Feedback Resolution Protocol

Whenever the user asks to review or address feedback on a Pull Request (e.g., `"Address the feedback on this PR"`), all five of the following steps must be executed automatically (see `.agent/skills/address-pr-feedback/SKILL.md`):

1. **Audit Diff, Implement Fixes & Push Code Changes:** Perform a Pre-Push Adversarial & Baseline-Regression Audit across the full `main...HEAD` diff (checking deleted baseline lines, sibling input/UI/kinematic paths, boundary conditions, and doc/skill alignment) so fixes resolve entire bug classes rather than narrow point-lines. Verify all Rule Group E gates (`cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`), and push commits to the PR branch.
2. **Reply to Addressed Feedback:** Post an in-thread reply to each addressed review comment stating **when** (commit SHA and timestamp/date) and **how** (files, functions, and mechanism) it was addressed.
3. **Reply to Unimplemented Feedback:** Post an in-thread reply to any feedback that was ignored or not implemented explaining **why** it was not implemented.
4. **Resolve All Review Conversations:** Resolve every addressed/replied review thread on the pull request via the GitHub GraphQL `resolveReviewThread` mutation.
5. **Post a Top-Level PR Summary Comment:** Post a summary comment on the PR detailing what changes were made and what was not addressed and why.

