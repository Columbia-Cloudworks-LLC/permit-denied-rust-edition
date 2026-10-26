# CLAUDE.md — Permit Denied (Rust / Bevy)

Workspace rules for the **Permit Denied** repository (`permit-denied-bevy`): a 2.5D isometric demolition sandbox on Bevy 0.15, Avian2D and `bevy_ecs_tilemap`, Rust edition 2024. Every change, crate and system must follow the six rule groups below.

## Reference implementation (TypeScript)

This repo is a **port**. The shipped TypeScript + Vite + PixiJS game is the reference, and this port aims for feature parity with it.

- **Local path:** `D:\repos\Permit Denied` (sibling folder `../Permit Denied`; there is no local `permit-denied-game` folder). GitHub `Columbia-Cloudworks-LLC/permit-denied-game`, live at <https://permitdenied.app>.
- **Read-only.** Never edit, commit, branch, install packages or run write-side scripts there (`npm run repo:preflight` fast-forwards and counts as a write). Reading files and `git log` / `git show` are fine.
- **Where to look:** its `README.md` is the source of truth for controls, scope and copy. Then `CLAUDE.md`, `docs/architecture.md` (cells, roofs, debris layers), `docs/campaign.md` (Time Challenge levels, targets, clocks), `docs/city-map-fabric.md` (urban bands), `docs/testing.md` (test tiers, recorded playthroughs). Behaviour lives in `src/sim/`, `src/structure/`, `src/world/`, `src/game/`, `src/vehicle/`; tuning constants in `src/game/constants.ts` and `src/game/campaign.ts`; building, site and vehicle JSON under `src/world/data/`.
- **Parity tracking:** [`docs/parity.md`](docs/parity.md) lists every area as Done / Partial / Missing / Different-by-design. Update the row when a change closes or opens a gap.
- **When they disagree:** match the reference's player-visible behaviour, numbers and copy unless `docs/parity.md` marks the item Different-by-design. The Rust architecture (Bevy ECS, crate DAG, Avian2D, zero-panic rules above) is deliberately different and is not a parity gap. The reference's "no ECS" stack lock applies only to that repo.
- **Copy rules carried over:** `BLADE UP` / `BLADE DOWN`; run-ending states `ENGINE COOKED` / `TRACK THROWN` / `COUNTY CLOCK`; title `PERMIT DENIED`, tagline "The County Said No." Never write Heemeyer, Granby, manifesto or wanted stars.

## Commands

```bash
cargo check --workspace                                  # fast type check
cargo clippy --workspace --all-targets -- -D warnings    # Gate 1
cargo test --workspace                                   # Gates 2, 3 & 4
cargo run                                                # play (F9 record, F10 replay, F4 colliders)
cargo run -- --scenario scenarios/ranch.ron              # windowed scenario run: PNGs + contact sheet + MATCH/DRIFT
cargo run -- --scenario scenarios/ranch.ron --headless   # same verdict, no window (telemetry JSON only)
```

## Record, replay and captures

The simulation is deterministic: the same build, OS, seed and tick-indexed inputs reproduce a run bit for bit (Rule B). `pd_replay` and the binary's `scenario_cli` / `replay_hud` build on that, mirroring the reference's Record Playthrough / Replay / MATCH-DRIFT flow (`TS:docs/testing.md#recorded-playthroughs`).

- **Scenario files** (`scenarios/*.ron`, format `permit-denied-scenario` v1): `seed`; `settings` (mode, level, season, weather, `test_map`, starting upgrades); change-only `controls: [(tick, left, right, blade)]`; `actions` (`Upgrade(at, choice)`, applied before tick `at` runs); `captures: [(tick, label)]` (state after `tick` ticks); `overlays` (`telemetry` = F3 panel, `colliders` = iso Avian outlines, `zoom` = capture camera only); `exit_tick`; and an optional `expect` digest. A recording is a scenario with `expect` filled in.
- **CLI:** `cargo run -- --scenario <file> [--out <dir>] [--headless] [--write-expect]`. Runs in lockstep (exactly one 60 Hz tick per frame). Output goes to `target/scenario-out/<name>/` by default: `<tick>-<label>.png`, `<tick>-<label>.telemetry.json`, `contact-sheet.png` and `summary.json`. Exit code 0 = MATCH or no `expect`, 1 = DRIFT (the drifted fields are printed), 2 = error. `--write-expect` stores the reached digest as `expect` and keeps the file's leading `//` comment block.
- **In game:** **F9** restarts the site and records from tick 0. **F9** again, or any restart (B, Tab, New Game), stops it and writes `scenarios/rec-<UTC>.ron`. **F10** replays the last recording (else the newest `scenarios/rec-*.ron`) with live drive and upgrade input ignored. The badge then shows `MATCH` or `DRIFT: <fields>`, and **F10** again exits. **F4** toggles collider outlines.
- **Ranch benchmark:** `scenarios/ranch.ron` (`test_map: Ranch`) is the canonical recording, playing the reference ranch test's input timeline. After a deliberate simulation change, regenerate its digest with `--headless --write-expect`, then run it windowed and look at the contact sheet before committing.
- **A DRIFT on the same build and OS is a bug.** Something in the sim is not tick-indexed. Bisect with captures, then compare the per-tick state of the two runs. Recordings from another build may drift legitimately; the payload names its build.
- **Tests:** `tests/determinism.rs`, `tests/replay.rs`, `tests/replay_hotkeys.rs` and `tests/ranch.rs` run under `cargo test`. Build headless apps with `permit_denied::headless_sim_app()` (lockstep, no window) and run files with `scenario_cli::run_headless`.

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

- **Forbidden operations.** `[workspace.lints.clippy]` in `Cargo.toml` denies these across all runtime crates (`pd_core`, `pd_dozer`, `pd_world`, `pd_destruction`, `pd_ai`, `pd_ui`, `pd_replay`) and the main binary. Never weaken or bypass them:
  - `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`, `clippy::todo`, `clippy::unimplemented`, `clippy::indexing_slicing`, `clippy::float_cmp`
  - `#[cfg(test)]` modules may `#![allow(...)]` / `#[allow(...)]` these only where a test assertion needs it; prefer non-panicking tests that return `Result`.
- **Arithmetic safety.** Use `checked_*` or `saturating_*` arithmetic for cash ledger updates, structural graph node indices, pool indices and grid coordinates.
- **Float sanitization.** Every function that computes physics impulses, differential steering torque, simulated Z-height collapse kinematics, or gauge needle rotations (`ENGINE HEAT`, `TRACK STRESS`) must check `is_finite()` before writing to a Bevy component. On `NaN`/`Infinity`, log a structured `bevy::log::warn!` and fall back to zero velocity or the previous frame's valid state.

## B. Bevy ECS & Scheduling Hygiene

- **Explicit system ordering.** Two systems that mutate the same Component or Resource must never share a schedule without an explicit `SystemSet` ordering constraint (`.before(...)`, `.after(...)` or `.chain()`).
- **Deterministic `FixedUpdate`.** Skid-steer kinematics (`avian2d`), hydraulic blade interactions (`BLADE UP` / `BLADE DOWN`) and structural collision checks run in `FixedUpdate` at 60 Hz (`pd_core::SIM_HZ`) with delta time clamped to `[0.0001, 0.05]`. Rendering, camera interpolation, tilemap animations and HUD animations (`Cash` odometer reels, the `Elapsed` display) run in `Update`.
- **Deterministic simulation (same seed + same inputs reproduce a run).** Anything that changes simulation state must be tick-indexed:
  - It runs in a `FixedUpdate` `PdSystemSet` (`SimInput` → `DozerKinematics` → `StructuralImpacts` → `DestructionGraph` → `DebrisLifecycle` → `CrowdAndTrafficAi` → `SimTickEnd`), all gated by `pd_core::sim_running`.
  - Live input reaches the sim only through a tick latch: `LiveDriveControl` → `TrackInput` in `SimInput`, and upgrade picks through `UpgradePickQueue`.
  - Cash, `Elapsed` and milestones advance in `SimTickEnd` from `SimTick`.
  - `Update` and render systems must never write sim components or resources.
  - Randomness comes only from `SiteSeed` via `SplitMix64`. Never use wall-clock time, `Entity::index()`, `std` `HashMap` iteration order or query/archetype order where visit order changes the result. Iterate in `SimOrdinal` / `AgentOrdinal` / `site_ordinal` order instead (`pd_core::fill_sim_order`).
  - Async work joins on a fixed tick, never "whenever it's ready" (`COLLAPSE_EVAL_LATENCY_TICKS`).
  - To stop the sim between ticks, use `SimHold` (gameplay: the Campaign modal) or `SimFreeze` (tooling: replay end, captures). The `FixedFirst` physics gate pauses Avian with them, and it must also zero the physics delta (Avian 0.2 otherwise steps once more).
  - Gate 4 enforces all of this.
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
4. `pd_replay` — record/replay, scenario format and digest. Reads `pd_core`, `pd_world`, `pd_dozer` and `pd_destruction`; nothing below may depend on it. UI that needs replay state (the REC / MATCH badge) lives in the binary (`src/replay_hud.rs`), not in `pd_ui`.

The binary crate (`permit_denied`, `src/lib.rs` + `src/main.rs`) sits on top. It assembles every crate (`add_simulation_plugins`, `add_presentation_plugins`, `add_debug_tooling_plugins`, `headless_sim_app`) and may depend on all of them.

## E. Defensive Verification Gates

Do not report a milestone or subsystem task as complete until all four gates pass:

1. **Gate 1 (`verify_no_panic_paths`):** `cargo clippy --workspace --all-targets -- -D warnings` passes with zero warnings or errors.
2. **Gate 2 (`test_kinematic_and_ledger_extremes`):** unit tests feed extreme/corrupted inputs (`dt = 0.0`, `dt = 999.0`, `NaN`/`INFINITY` vectors, `u32::MAX` cash additions, disconnected single-node buildings) into core functions and assert clean `Err(...)` or clamped `Ok(...)` without panicking.
3. **Gate 3 (`test_idempotent_site_restart`):** a headless Bevy world test spawns a city block, triggers a building collapse, dispatches `Restart Site` mid-collapse, ticks 10 frames, and verifies that `SessionEpoch` advances, stale async tasks are discarded, and zero orphaned site entities remain.

4. **Gate 4 (deterministic replay):** `tests/determinism.rs` (fresh vs fresh, dirty session history vs fresh, Campaign modal pause length, and no ambiguous `FixedUpdate` ordering) plus the replay tests (`tests/replay.rs`, `tests/replay_hotkeys.rs`, `tests/ranch.rs`: `scenarios/*.ron` and live recordings replay to MATCH) all pass.

Gates 2, 3 and 4 all run under `cargo test --workspace`.

## F. Pull Request Feedback Resolution Protocol

When asked to review or address feedback on a pull request (e.g. "Address the feedback on this PR"), carry out all five steps using the `address-pr-feedback` skill. The request itself authorizes pushing to the PR branch and posting the replies/comments below.

1. **Audit, fix and push.** Run a pre-push adversarial & baseline-regression audit across the full `main...HEAD` diff (deleted baseline lines, sibling input/UI/kinematic paths, boundary conditions, doc/skill alignment) so fixes resolve whole bug classes, not just the cited line. Pass the Rule E gates (`cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`), then push to the PR branch.
2. **Reply to addressed feedback** in-thread, stating **when** (commit SHA and timestamp/date) and **how** (files, functions, mechanism) it was addressed.
3. **Reply to unimplemented feedback** in-thread, explaining **why** it was not implemented.
4. **Resolve every addressed/replied review thread** via the GitHub GraphQL `resolveReviewThread` mutation.
5. **Post a top-level PR summary comment** covering what changed and what was not addressed and why.
