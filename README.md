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
| **Toggle Hydraulic Blade (`BLADE UP` / `BLADE DOWN`)** | **`Space`** | **`Left Shift`**, **`R`**, or **`F`** |
| **Restart Current Demolition Site** | **`B`** | Click `Pause` -> `Restart Site` |
| **Cycle Next Campaign Level (`1/7` – `7/7`)** | **`Tab`** | Click `New Game` -> `Level` |
| **Pause / Resume Game** | **`Esc`** | Click `Pause` in Top Bar |
| **Toggle Live Debug Telemetry Overlay** | **`F3`** | Click `Debug` in Top Bar |

### Core Mechanics
* **`BLADE UP` vs. `BLADE DOWN`:**
  * Keep the blade **UP** (`● BLADE UP` in green on the bottom-right cab dashboard) for higher travel speed and lower engine/track strain.
  * Drop the blade **DOWN** (`● BLADE DOWN` in amber) to multiply structural shear damage against load-bearing pillars and walls, tear up concrete foundations, and **crush sleeping rubble piles into flat ground scars**.
* **Structural Collapse & Cash Upgrades:**
  * Knocking out ground-floor load-bearing pillars and walls severs upper stories from the foundation, triggering a 2.5D cascading collapse.
  * Demolishing structures, interior furniture (cash registers, forklifts, refrigerators, toilets), propane tanks, fire hydrants, and vehicles awards cash on the **6-digit rolling mechanical odometer** (`CASH $000,000`).
  * Earning demolition cash automatically scales your three live dozer upgrades shown on the bottom dashboard: **`AR400 Edge`** (blade shear multiplier), **`Charge-Air Kit`** (horsepower & cooling), and **`Aux Ram`** (hydraulic pushing force).
* **Engine Heat & Track Stress:**
  * In **Campaign Mode**, pushing heavy masonry with the blade down builds **`ENGINE HEAT`** and **`TRACK STRESS`**. If either hits `100%`, the dozer enters temporary limp mode until it cools below recovery thresholds.
  * In **Sandbox Mode**, breakdown immunity is active so you can demolish without overheating.

---

## 4. Workspace & Crate Architecture (Strict DAG)

```text
permit-denied-bevy/
├── GEMINI.md                   # Global Zero-Panic & ECS Scheduling Rules
├── .agent/skills/              # 6 On-Demand Defensive Agent Skills
├── Cargo.toml                  # Workspace root + strict Clippy lints
├── assets/
│   ├── atlases/                # 2.5D sprite sheets (buildings, furniture, vehicles, NPCs)
│   ├── tiles/                  # Isometric road, terrain, and seasonal tilemaps
│   ├── prefabs/                # .ron building definitions & structural node graphs
│   ├── audio/                  # Diesel idle/rev loops, hydraulic hiss, crunch SFX
│   └── fonts/                  # Monospace, 7-segment LED, and stencil fonts
└── crates/
    ├── pd_core/                # Game states, WorldPose, IsoExtrudedBox, SessionEpoch, CashLedger
    ├── pd_dozer/               # Skid-steer physics, Blade Up/Down state, 2.5D dozer assembly
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
