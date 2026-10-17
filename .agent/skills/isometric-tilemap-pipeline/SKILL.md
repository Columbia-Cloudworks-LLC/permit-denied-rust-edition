---
name: isometric-tilemap-pipeline
description: >-
  2.5D isometric projection math, chunked tilemap streaming (bevy_ecs_tilemap),
  seasonal palette shaders (Spring, Summer, Autumn, Winter), road auto-tiling,
  and interior cutaway masks. Use this skill when working on pd_world, terrain
  rendering, tilemaps, cameras, or shaders.
---

# Isometric Tilemap Pipeline Skill (`isometric-tilemap-pipeline`)

Use this playbook when implementing or modifying `pd_world` tilemaps, isometric projection, seasonal palettes, and cutaway visibility.

## 1. Core Defensive Invariants

1. **Out-of-Bounds Grid Protection:**
   * All tile and chunk coordinate lookups must use checked conversion (`u32::try_from`, `.checked_add`, `.checked_sub`) and bounds validation against `MapDimensions` before querying `TileStorage` or slice buffers.
2. **Texture Atlas Index Validation:**
   * Every sprite/tile atlas index must be validated against `max_atlas_index`. If an index is out of range, substitute `FALLBACK_ERROR_TILE_INDEX` (0) and log a warning.
3. **Z-Sorting Stability (No Z-Fighting):**
   * Isometric 2D sorting must compute a deterministic depth `z`:
     `z = base_layer_z - (iso_y * 0.001) - (iso_x * 0.00001) + (simulated_height * 0.01)`
   * Validate that `z.is_finite()` and lies within camera near/far clip planes (`[-900.0, 900.0]`).
4. **Cutaway Mask Hygiene:**
   * When the dozer breaches an exterior wall or enters a building footprint, upper-floor/roof alpha fades smoothly using clamped `[0.0, 1.0]` interpolation to reveal interior flooring tiles and furniture sprites.

## 2. Verification Gate

* Verify zero out-of-bounds panics on negative or `i32::MAX` grid coordinates.
* Run `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test -p pd_world`.
