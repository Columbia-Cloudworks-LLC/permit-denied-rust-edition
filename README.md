# Permit Denied — 2.5D Isometric Demolition Sandbox (Rust / Bevy)

**Permit Denied** (*"The County Said No."*) is an isometric, physics-driven demolition sandbox developed by Columbia Cloudworks LLC, rewritten from the ground up in **Rust** and **Bevy 0.15** with **Avian2D** and **`bevy_ecs_tilemap`**.

---

## Workspace & Crate Architecture (Strict DAG)

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
    ├── pd_core/                # Game states, SessionEpoch, CashLedger, CashPopupPool, Season/Weather
    ├── pd_dozer/               # Skid-steer physics, Blade Up/Down state, Heat/Stress/Upgrades
    ├── pd_world/               # Isometric tilemap math, seasonal palettes, road spline graph, spatial grid
    ├── pd_destruction/         # Building graph, async BFS collapse tasks, 500-cap 3-tier debris pool
    ├── pd_ai/                  # Spatial hash crowd panic AI & spline-to-physics traffic vehicles
    └── pd_ui/                  # Industrial Top/Bottom HUD bars, 6-digit rolling odometer, gauges & modals
```

---

## Defensive Verification Gates

1. **Gate 1 (`verify_no_panic_paths`):**
   ```bash
   cargo clippy --workspace --all-targets -- -D warnings
   ```
2. **Gate 2 (`test_kinematic_and_ledger_extremes`) & Gate 3 (`test_idempotent_site_restart`):**
   ```bash
   cargo test --workspace
   ```
