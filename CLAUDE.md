# CLAUDE.md — Permit Denied (Rust / Bevy)

Workspace rules for the **Permit Denied** repository (`permit-denied-bevy`): a 2.5D isometric demolition sandbox on Bevy 0.15, Avian2D and `bevy_ecs_tilemap`, Rust edition 2024. Every change, crate and system must follow the six rule groups below.

## Commands

```bash
cargo check --workspace                                  # fast type check
cargo clippy --workspace --all-targets -- -D warnings    # Gate 1
cargo test --workspace                                   # Gates 2 & 3
cargo run                                                # play
```

## Project skills (`.claude/skills/`)

Load the matching skill before working in a subsystem:

| Skill | Use for |
| :--- | :--- |
| `rust-defensive-ecs` | Any Bevy plugin, system, resource, async task or state transition in any crate |
| `dozer-kinematics-physics` | `pd_dozer`: skid-steer, blade state machine, upgrades, gauges, anti-wedging |
| `destruction-graph-pooling` | `pd_destruction`: building graphs, collapse BFS, debris pools, `PileField`, prop profiles |
| `crowd-traffic-spatial-ai` | `pd_ai`: spatial hash, pedestrian flee steering, traffic vehicles |
| `isometric-tilemap-pipeline` | `pd_world`: iso projection, tilemaps, Z-sorting, seasonal palettes, cutaways |
| `industrial-hud-state` | `pd_ui` and `pd_core` ledger/popup/restart contracts |
| `address-pr-feedback` | Any request to review, handle or address feedback on a pull request |

---

## A. Compiler & Linter Hardening (Zero-Panic Policy)

- **Forbidden operations.** `[workspace.lints.clippy]` in `Cargo.toml` denies these across all runtime crates (`pd_core`, `pd_dozer`, `pd_world`, `pd_destruction`, `pd_ai`, `pd_ui`) and the main binary. Never weaken or bypass them:
  - `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`, `clippy::todo`, `clippy::unimplemented`, `clippy::indexing_slicing`, `clippy::float_cmp`
  - `#[cfg(test)]` modules may `#![allow(...)]` / `#[allow(...)]` these only where a test assertion needs it; prefer non-panicking tests that return `Result`.
- **Arithmetic safety.** Use `checked_*` or `saturating_*` arithmetic for cash ledger updates, structural graph node indices, pool indices and grid coordinates.
- **Float sanitization.** Every function that computes physics impulses, differential steering torque, simulated Z-height collapse kinematics, or gauge needle rotations (`ENGINE HEAT`, `TRACK STRESS`) must check `is_finite()` before writing to a Bevy component. On `NaN`/`Infinity`, log a structured `bevy::log::warn!` and fall back to zero velocity or the previous frame's valid state.

## B. Bevy ECS & Scheduling Hygiene

- **Explicit system ordering.** Two systems that mutate the same Component or Resource must never share a schedule without an explicit `SystemSet` ordering constraint (`.before(...)`, `.after(...)` or `.chain()`).
- **Deterministic `FixedUpdate`.** Skid-steer kinematics (`avian2d`), hydraulic blade interactions (`BLADE UP` / `BLADE DOWN`) and structural collision checks run in `FixedUpdate` with delta time clamped to `[0.0001, 0.05]`. Rendering, camera interpolation, tilemap animations and HUD animations (`Cash` odometer reels, `Elapsed` timer) run in `Update`.
- **Safe query access.** Use only non-panicking query methods (`get`, `get_mut`, `get_single`, `get_single_mut`, `iter`, `iter_mut`) and handle `Err` / `None` with an early return plus structured telemetry logging. Never call `single()` or `single_mut()`.
- **Epoch-guarded async & commands.** Whenever an `Entity` is passed into an `AsyncComputeTaskPool` task or deferred command (e.g. structural collapse BFS), the payload must carry the current `SessionEpoch(u64)`. Before applying deferred mutations, the receiving system must verify that the epoch matches **and** that the entity still exists (`commands.get_entity(entity)`), so `Restart Site` / `New Game` can't race stale work.

## C. Memory Budgets & Object Pooling

- **No per-frame heap churn in hot loops.** `Update` and `FixedUpdate` systems must not allocate new `Vec` or `HashMap` instances in hot loops; use pre-allocated scratch buffers in `Local<T>` parameters or fixed-capacity resources.
- **Hard entity ceilings.** Dynamic entities (falling building chunks, shattered furniture, floating `+$X` popups, particle bursts) are governed by ring-buffer pools:
  - Active physics debris: max **500** dynamic bodies (`recycle_oldest_debris` strips the collider and bakes the chunk into the static ground rubble layer).
  - Floating cash popups: max **50** active (spatial/temporal coalescing within `100ms` and a `2.0m–3.0m` radius).

## D. Crate Dependency Hierarchy (Strict DAG)

Circular dependencies are forbidden. Crates obey this DAG:

1. `pd_core` — bottom layer: game states, `SessionEpoch`, `CashLedger`, `CashPopupPool`, `Season`/`Weather`/`CampaignLevel` configs, float/math guards, custom error enums. Depends on **nothing** internal.
2. `pd_world` and `pd_dozer` — depend **only** on `pd_core`.
3. `pd_destruction` and `pd_ai` — depend **only** on `pd_core` and `pd_world`.
4. `pd_ui` — reads state from `pd_core` and `pd_dozer` via read-only queries/events.

## E. Defensive Verification Gates

Do not report a milestone or subsystem task as complete until all three gates pass:

1. **Gate 1 (`verify_no_panic_paths`):** `cargo clippy --workspace --all-targets -- -D warnings` passes with zero warnings or errors.
2. **Gate 2 (`test_kinematic_and_ledger_extremes`):** unit tests feed extreme/corrupted inputs (`dt = 0.0`, `dt = 999.0`, `NaN`/`INFINITY` vectors, `u32::MAX` cash additions, disconnected single-node buildings) into core functions and assert clean `Err(...)` or clamped `Ok(...)` without panicking.
3. **Gate 3 (`test_idempotent_site_restart`):** a headless Bevy world test spawns a city block, triggers a building collapse, dispatches `Restart Site` mid-collapse, ticks 10 frames, and verifies that `SessionEpoch` advances, stale async tasks are discarded, and zero orphaned site entities remain.

Gates 2 and 3 both run under `cargo test --workspace`.

## F. Pull Request Feedback Resolution Protocol

When asked to review or address feedback on a pull request (e.g. "Address the feedback on this PR"), carry out all five steps using the `address-pr-feedback` skill. The request itself authorizes pushing to the PR branch and posting the replies/comments below.

1. **Audit, fix and push.** Run a pre-push adversarial & baseline-regression audit across the full `main...HEAD` diff (deleted baseline lines, sibling input/UI/kinematic paths, boundary conditions, doc/skill alignment) so fixes resolve whole bug classes, not just the cited line. Pass the Rule E gates (`cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`), then push to the PR branch.
2. **Reply to addressed feedback** in-thread, stating **when** (commit SHA and timestamp/date) and **how** (files, functions, mechanism) it was addressed.
3. **Reply to unimplemented feedback** in-thread, explaining **why** it was not implemented.
4. **Resolve every addressed/replied review thread** via the GitHub GraphQL `resolveReviewThread` mutation.
5. **Post a top-level PR summary comment** covering what changed and what was not addressed and why.
