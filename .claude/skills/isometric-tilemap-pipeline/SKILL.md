---
name: isometric-tilemap-pipeline
description: >-
  Defensive invariants for pd_world in Permit Denied: 2.5D isometric
  projection math and deterministic Z-sorting, chunked tilemap streaming
  (bevy_ecs_tilemap) with checked grid/atlas lookups, seasonal palette shaders
  (Spring, Summer, Autumn, Winter), road auto-tiling, and interior cutaway
  masks. Use when working on pd_world, terrain rendering, tilemaps, isometric
  projection, cameras, depth sorting, or shaders.
---

# Isometric Tilemap Pipeline

Use this playbook when implementing or modifying `pd_world` tilemaps, isometric projection, seasonal palettes and cutaway visibility.

## 1. Core defensive invariants

1. **Out-of-bounds grid protection:**
   - All tile and chunk coordinate lookups use checked conversion (`u32::try_from`, `.checked_add`, `.checked_sub`) and bounds validation against `MapDimensions` before querying `TileStorage` or slice buffers.
2. **Texture atlas index validation:**
   - Validate every sprite/tile atlas index against `max_atlas_index`. If out of range, substitute `FALLBACK_ERROR_TILE_INDEX` (`0`) and log a warning.
3. **Z-sorting stability (no Z-fighting):**
   - Isometric 2D sorting computes a deterministic depth:
     `z = base_layer_z - (iso_y * 0.001) - (iso_x * 0.00001) + (simulated_height * 0.01)`
   - Validate that `z.is_finite()` and lies within the camera near/far clip planes (`[-900.0, 900.0]`).
4. **Cutaway mask hygiene:**
   - When the dozer breaches an exterior wall or enters a building footprint, upper-floor/roof alpha fades smoothly using clamped `[0.0, 1.0]` interpolation to reveal interior flooring tiles and furniture sprites.

## 2. Verification gate

- Tests verify zero out-of-bounds panics on negative or `i32::MAX` grid coordinates.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test -p pd_world` pass.
