# Permit Denied — 2.5D Isometric Demolition Sandbox (Rust / Bevy)

**Permit Denied** (*"The County Said No."*) is an isometric, physics-driven demolition sandbox developed by Columbia Cloudworks LLC, built in **Rust** and **Bevy 0.15** with **Avian2D** physics and **`bevy_ecs_tilemap`**.

---

## 1. Prerequisites (New to Rust?)

To compile and run **Permit Denied**, you only need the standard **Rust toolchain** (`rustup`, `rustc`, and `cargo` — Rust's built-in package manager and build tool).

1. **Install Rust:**
   - **Windows:** Download and run [`rustup-init.exe`](https://rustup.rs/) (requires [Visual Studio C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the *"Desktop development with C++"* workload selected).
   - **macOS / Linux:** Run:
     ```bash
     curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
     ```
2. **Verify your installation:**
   Open a terminal (PowerShell, Command Prompt, or bash) and run:
   ```bash
   rustc --version
   cargo --version
   ```

---

## 2. How to Compile & Play the Game

Open your terminal in the repository root (`permit-denied-bevy`) and run:

```bash
cargo run
```

* **First Build Note:** The very first time you run `cargo run`, Cargo will automatically download and compile Bevy, Avian2D, and all dependencies. This initial build can take **2–5 minutes** depending on your CPU. Subsequent builds only recompile your changes and finish in a few seconds!
* **Already Optimized Dev Profile:** `Cargo.toml` is pre-configured with `opt-level = 3` for all dependencies even in `dev` mode, so `cargo run` runs smoothly at 60 FPS out of the box.
* **Full Release Build (Optional):** If you want to build a standalone maximum-performance binary:
  ```bash
  cargo run --release
  ```
  *(The compiled executable will be placed at `target/release/permit_denied.exe` on Windows or `target/release/permit_denied` on macOS/Linux).*

---

## 3. Controls & Gameplay Guide

### Starting a Session
* When the game window opens on the **COUNTY BUILDING DEPARTMENT — APPLICATION FOR DEMOLITION** Title Screen:
  * Press **`Enter`** or click **`⏻ PLAY`** to launch directly into the demolition site.
  * Or click **`New Game`** to configure **Mode** (`Campaign` vs. `Sandbox`), **Campaign Level** (`1/7 County` through `7/7 Governor's Mansion`), **Season** (`Random`, `Spring`, `Summer`, `Autumn`, `Winter`), and **Weather** (`On` / `Off`), then click **`Start Game`**.

### Bulldozer Controls
| Action | Primary Keys | Alternate Keys |
| :--- | :--- | :--- |
| **Drive Forward / Reverse** | **`W`** / **`S`** | **`Up Arrow`** / **`Down Arrow`** |
| **Pivot / Steer Left & Right** | **`A`** / **`D`** | **`Left Arrow`** / **`Right Arrow`** |
| **Left Track Only (Forward / Reverse)** | **`Q`** / **`Z`** | — |
| **Right Track Only (Forward / Reverse)** | **`E`** / **`C`** | — |
| **Lower Hydraulic Blade / Powered Surge (`BLADE DOWN` while held)** | **`Space`** (Hold / Tap) | **`Left Shift`**, **`R`**, or **`F`** |
| **Select / Apply Dozer Upgrade (`AR400` / `Charge-Air` / `Aux Ram`)** | **`1`** / **`2`** / **`3`** | **`Numpad 1`–`3`** or Upgrade Modal Buttons |
| **Restart Current Demolition Site** | **`B`** | Click `Pause` -> `Restart Site` |
| **Cycle Next Campaign Level (`1/7` – `7/7`)** | **`Tab`** | Click `New Game` -> `Level` |
| **Pause / Resume Game** | **`Esc`** | Click `Pause` in Top Bar |
| **Toggle Live Debug Telemetry Overlay** | **`F3`** | Click `Debug` in Top Bar |
| **Toggle Collider Outlines** | **`F4`** | — |
| **Record Playthrough (restarts the site) / Stop & Save to `scenarios/`** | **`F9`** | Any restart also stops & saves |
| **Replay Last Recording (MATCH / DRIFT) / Exit Replay** | **`F10`** | — |

### Core Mechanics
* **`BLADE UP` vs. `BLADE DOWN` (Timed Powered Surge & Hold):**
  * Release the blade key to travel with the blade **UP** (`● BLADE UP` in green on the bottom-right cab dashboard) for full `1.00x` cruising speed and lower engine/track strain.
  * Press or hold **`Space`** (or **`Left Shift`**, **`R`**, **`F`**) to drop the blade **DOWN** (`● BLADE DOWN` in amber):
    * Pressing the blade key when off cooldown triggers a timed **powered blade push surge** (`0.72s` base duration scaled by **`Aux Ram`**, up to `1.12x` top speed followed by a `2.15s` cooldown) and a blade-slam impact bonus on the landing tick.
    * Holding the blade key keeps the blade lowered (`0.74x` sustained travel drag after the surge ends) to multiply structural shear damage against load-bearing pillars and walls, tear up concrete foundations, and **crush sleeping rubble piles into flat ground scars**. Releasing the key raises the blade once any active powered push timer finishes.
* **Structural Collapse & Interactive Milestone Upgrades:**
  * Knocking out ground-floor load-bearing pillars and walls severs upper stories from the foundation, triggering a 2.5D cascading collapse.
  * Demolishing structures, interior furniture (cash registers, forklifts, refrigerators, toilets), propane tanks, fire hydrants, and vehicles awards cash on the **6-digit rolling mechanical odometer** (`CASH $000,000`).
  * **Campaign Mode Milestones:** Crossing demolition cash milestones (`$350`, `$900`, `$1,600`, and every `+$900` thereafter) pauses the simulation and opens the **Equipment Upgrade Milestone** modal until you choose one upgrade via **`1`**, **`2`**, **`3`** (or **`Numpad 1`–`3`** / modal buttons):
    * **`1 · AR400 Edge (+42%)`** — Increases structural node shear damage per blade impact.
    * **`2 · Charge-Air Kit (+28%)`** — Increases forward/reverse top speed, acceleration, and engine heat cooling.
    * **`3 · Aux Ram (+35%)`** — Increases timed powered blade push duration, surge force, recoil resistance, and track stress cooling.
  * Advancing to the next Campaign level (**`Tab`**) snapshots and carries your installed upgrades forward, whereas retrying the current site (**`B`** or `Pause` -> `Restart Site`) restores the upgrades you had upon entering that level.
  * **Sandbox Mode Hotkeys:** Press **`1`**, **`2`**, or **`3`** (or **`Numpad 1`–`3`**) at any time to apply upgrades immediately without pausing or requiring cash milestones.
* **Engine Heat & Track Stress:**
  * In **Campaign Mode**, pushing heavy masonry with the blade down builds **`ENGINE HEAT`** and **`TRACK STRESS`**. If either hits `100%`, the dozer enters temporary limp mode until it cools below recovery thresholds.
  * In **Sandbox Mode**, breakdown immunity is active so you can demolish without overheating.

---

## 4. Workspace & Crate Architecture (Strict DAG)

```text
permit-denied-bevy/
├── CLAUDE.md                   # Global Zero-Panic & ECS Scheduling Rules (Claude Code)
├── .claude/skills/             # On-Demand Defensive Claude Code Skills
├── Cargo.toml                  # Workspace root + strict Clippy lints
├── assets/
│   ├── atlases/                # 2.5D sprite sheets (buildings, furniture, vehicles, NPCs)
│   ├── tiles/                  # Isometric road, terrain, and seasonal tilemaps
│   ├── prefabs/                # .ron building definitions & structural node graphs
│   ├── audio/                  # Diesel idle/rev loops, hydraulic hiss, crunch SFX
│   └── fonts/                  # Monospace, 7-segment LED, and stencil fonts
└── crates/
    ├── pd_core/                # Game states, WorldPose, IsoExtrudedBox, SessionEpoch, CashLedger
    ├── pd_dozer/               # Skid-steer physics, timed blade push, milestone upgrades, 2.5D dozer assembly
    ├── pd_world/               # 2:1 isometric diamond meshes, 2.5D extruded box renderer, sun lighting
    ├── pd_destruction/         # Building graph, async BFS collapse tasks, 500-cap 3-tier debris pool
    ├── pd_ai/                  # Spatial hash crowd panic AI & spline-to-physics traffic vehicles
    └── pd_ui/                  # Industrial Top/Bottom HUD bars, 6-digit rolling odometer, gauges & modals
```

---

## 5. Useful Cargo Commands & Verification Gates

* **Check code without building an executable (fastest syntax/type check):**
  ```bash
  cargo check --workspace
  ```
* **Gate 1 — Run the Rust Linter (`clippy`) with Zero-Panic rules enforced:**
  ```bash
  cargo clippy --workspace --all-targets -- -D warnings
  ```
* **Gate 2 & Gate 3 — Run all unit and headless integration tests across every crate:**
  ```bash
  cargo test --workspace
  ```
