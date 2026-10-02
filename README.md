# Permit Denied — Milestone 1: Mechanical Sandbox

A mechanical demolition sandbox built from scratch in **Rust** using the **Bevy Engine (v0.15)**, **Avian 3D (v0.2)** physics, and **Leafwing Input Manager**.

---

## 🏗️ Architecture & Features

### 1. Accurate Heavy Locomotion & Skid-Steering (`src/dozer.rs`)
- **Realistic Mass & Inertia**: Komatsu D355A derivative (~65 metric tons: 60,000 kg chassis + 5,000 kg blade).
- **High Rotational Inertia**: Realistic yaw inertia prevents instant spinning and ensures deliberate, weighty heavy equipment maneuvering.
- **Skid-Steering Simulation**:
  - Independent track throttles (`left_throttle`, `right_throttle`).
  - Anisotropic lateral friction locks the tracks onto their rails and eliminates sideways slippage.
  - In-place pivot capability when tracks drive in opposing directions ($+1, -1$).
  - Authentic ~1.5s ramp to top speed (~4.5 m/s or 16 km/h) with low longitudinal rolling drag, allowing momentum to carry the dozer forward on release.

### 2. Active Hydraulic Blade System (`src/dozer.rs`)
- Elevation range: Ground scrape ($0.0\text{ m}$) to elevated bumper ($+1.8\text{ m}$).
- 15° forward attack angle: Downward force digs into foundations; raised angle scoops and scatters collapsed chunks.
- Synchronized physical collider and visual arms actuated smoothly along hydraulic elevation axes.

### 3. Impulse Damage & Fracture Mechanics (`src/collision.rs`)
- Kinetic impulse formula:
  $$\mathbf{J} = \Delta \mathbf{p} = m_{\text{eff}} \cdot \mathbf{v}_{\text{rel}}$$
- Contact evaluation:
  - If $J_n \ge \text{YieldThreshold}_{\text{material}}$: The structural node fractures, disconnects from the structural graph, converts into a dynamic rigid body chunk, and scatters with momentum transfer.
  - If $J_n < \text{YieldThreshold}_{\text{material}}$: The wall absorbs the blow, stopping or slowing the dozer without fracturing the element (feeling of hitting reinforced bedrock).

### 4. Graph Integrity & BFS Structural Collapse (`src/building.rs`)
- 6-directional orthogonal adjacency graph ($\pm X, \pm Y, \pm Z$).
- Ground anchor nodes at layer $Y = 0$.
- Periodic 10 Hz Breadth-First Search (BFS) graph reachability cycle:
  - Traverses the intact structural graph starting from remaining ground anchors.
  - Detects orphaned blocks whose support load paths have been eliminated.
  - Converts orphaned cells to dynamic falling bodies that collapse under gravity onto the ground.

### 5. Performance Budget & Debris Management (`src/debris.rs`)
- **400 Dynamic Debris Cap**: Caps simultaneous dynamic rigid bodies in the physics world to protect mid-tier hardware.
- **Aggressive Sleep Threshold**: Linear velocity $< 0.15\text{ m/s}$ and angular velocity $< 0.1\text{ rad/s}$ for $> 0.5\text{ s}$ puts chunks into sleep.
- **Settling / Freezing**: Debris resting below floor level or stationary for $> 10\text{ s}$ bakes into static colliders, freeing the solver for continuous 60+ FPS.

### 6. Isometric Tactical Camera & Trauma Screen Shake (`src/camera.rs`)
- Isometric perspective with smooth follow damping tracking the dozer's momentum.
- Screen-space camera shake using non-linear trauma response ($\text{shake} = \text{trauma}^2$) and exponential decay.

### 7. Data-Driven Building Spec (`assets/buildings/sample_hardware_store.building.ron`)
- Declarative Rusty Object Notation (.ron) format defining materials, yield thresholds, densities, colors, and grid layer layouts.
- Can be hot-reloaded during gameplay by pressing **[B]**.

---

## 🎮 Controls

| Action | Keyboard | Gamepad |
| :--- | :--- | :--- |
| **Drive Forward / Reverse** | `W` / `S` (or `Up` / `Down`) | Left Stick / D-Pad Up/Down |
| **Steer / Pivot in Place** | `A` / `D` (or `Left` / `Right`) | Left Stick / D-Pad Left/Right |
| **Left Track Direct** | `Q` (Forward) / `Z` (Reverse) | — |
| **Right Track Direct** | `E` (Forward) / `C` (Reverse) | — |
| **Raise Blade** | `Space` or `R` | Right Trigger (RT / R2) |
| **Lower Blade** | `Left Shift` or `F` | Left Trigger (LT / L2) |
| **Rebuild / Reset Warehouse** | `B` | North (Y / Triangle) |

---

## 🚀 Running the Game

Run with cargo from the project root:

```bash
cargo run
```

Or execute the built binary directly:

```bash
./target/debug/permit_denied.exe
```
