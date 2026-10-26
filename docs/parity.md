# Feature parity with the TypeScript reference

The TypeScript game at `D:\repos\Permit Denied` ([permit-denied-game](https://github.com/Columbia-Cloudworks-LLC/permit-denied-game), live at <https://permitdenied.app>) is the reference implementation. This file tracks how far the Rust / Bevy port has reached toward it. CLAUDE.md, under "Reference implementation", explains the relationship.

**Snapshot:** reference `1.0.49` at `e374d87`. Port at `eca708d`. Audited 2026-10-25. Record/replay, determinism and ranch rows updated 2026-10-25 on `feat/record-replay`.

| Status | Meaning |
| --- | --- |
| **Done** | Matches the reference's player-visible behaviour closely enough to stop tracking. |
| **Partial** | Some of it exists, but the behaviour, numbers or scope differ. The note says how. |
| **Missing** | Nothing equivalent in the port yet. |
| **Different-by-design** | Deliberately different. Not a gap. Rows marked *(confirm)* are divergences the port introduced without a written decision. They need a keep-or-match call. |

Paths prefixed `TS:` are in the reference repo. All other paths are in this repo. Most Rust code sits in one `lib.rs` per crate, so Rust references name the function or type.

## Where the port stands

The port has a solid core loop. The dozer drives with blade surge, upgrades, heat and track stress. Per-cell buildings collapse through an epoch-guarded BFS. Debris runs through a 500-body pool, the `PileField` and collapsed sites. The HUD and modals work, and there are headless restart tests. The simulation is now deterministic on a 60 Hz tick, with record/replay (F9 / F10, MATCH / DRIFT), a scenario CLI with PNG captures and contact sheets, and a ranch benchmark. Three things are still missing, and most other parity work depends on them:

1. **Seeded layouts.** `SiteSeed` exists and drives the season and weather, but every level still spawns the same three procedural buildings at fixed offsets. Retry-same-layout and the campaign's "same seed, same town" contract need seeded generation.
2. **No town.** There are no roads beyond an 8-node rectangle, no parcels, no terrain and no building packages. The reference has 106 authored building packages, 14 sites and 12 vehicle definitions.
3. **No Time Challenge.** The reference counts a clock down and ends the run. The port only counts elapsed time up, and has no landmark objective, dollar target or results screen.

---

## Simulation and destruction

| Item | Status | Note | TS | Rust |
| --- | --- | --- | --- | --- |
| Simulation in world coords, iso projection render-only | Done | `WorldPose` is the sim truth. `project_world_poses_to_isometric_transform_system` projects. | `TS:src/world/iso.ts`, `TS:src/render/drawIso.ts` | `pd_core::WorldPose`, `pd_world::project_world_poses_to_isometric_transform_system` |
| Fixed 60 Hz step | Done | `Time::<Fixed>::from_hz(SIM_HZ)` with `SIM_HZ = 60`. `SimTick` counts ticks, and `Elapsed`, cash and milestones run on it. Bevy's `Time<Virtual>` 250 ms `max_delta` plays the role of the reference's per-frame step cap. | `TS:src/game/fixedStep.ts`, `TS:src/game/constants.ts` | `pd_core::{SIM_HZ, SimTick, sim_running}` |
| Determinism (same seed + inputs reproduce a run) | Done | Bit-exact on the same build and OS. Covered by `tests/determinism.rs` (fresh vs fresh, dirty session history, Campaign modal pause length, `FixedUpdate` ambiguity detection) and `tests/replay.rs`. Collapse BFS results land on exactly dispatch tick + 1 (`COLLAPSE_EVAL_LATENCY_TICKS`); no join has had to block so far. Avian2D runs **without** `enhanced-determinism`, because our own `f32::sin` / `cos` already make replay same-OS only. Revisit if Linux CI is added that replays Windows-recorded scenarios. | `TS:src/game/rng.ts`, `TS:src/sim/soak.test.ts` | `pd_core::{SiteSeed, SplitMix64, SimOrdinal}`, `pd_destruction::poll_structural_collapse_tasks_system` |
| Per-cell structural grid with load-bearing ground supports | Partial | A node graph per cell, with walls, pillars and floors, cladding HP and BFS orphan detection. Buildings come from `build_procedural_building_graph(stories, half)`, not from authored layouts. | `TS:src/structure/building.ts`, `TS:src/structure/coreCollapse.ts` | `pd_destruction::StructuralNode`, `evaluate_orphaned_nodes_bounded` |
| Damage stays on the wall you hit (per-cell breach) | Done | Covered by `test_per_cell_breaching_opens_south_wall_doorway_while_other_walls_block`. | `TS:src/sim/coreImpact.ts` | `src/lib.rs::evaluate_dozer_structural_impacts_system` |
| Unsupported floors sag, crack and fall | Partial | Orphaned nodes detach into falling debris with Z-drop kinematics. The reference's staged sag, crack and fall animation is absent, and so is cross-building chain collapse (a falling bay leaning into a neighbour). | `TS:src/structure/coreCollapse.ts`, `TS:src/render/coreCollapseDraw.ts` | `pd_destruction::poll_structural_collapse_tasks_system` |
| Roof bays: gable, shed and flat sag, hinge and drop | Partial | `RoofStyle` × `RoofBayState::Sagging` exists, and the ranch scenario shows the sequence in the sim: the center bay sags from tick 22 hinged toward the breach and drops at tick 52, then the end bays follow by tick 116. It is not visible, though: the cutaway turns the whole roof translucent at the first breach (see Interior cutaway). Missing: sawtooth, industrial and irregular-strip roofs, plus "most support gone, all sections fail together". | `TS:src/structure/roof.ts` | `pd_destruction::build_roof_bays_for_nodes`, `step_building_roofs_and_collapse_sites_system` |
| Materials and steel-column cladding | Partial | `MaterialKind` covers HP, density, crushability and colours, and pillar cladding soaks damage first. The reference's construction presets (timber-house, brick-mixed-use, steel-hall, concrete-bearing, glasshouse) and separate facade material are absent. | `TS:src/structure/materials.ts`, `TS:src/world/data/shared/construction/` | `pd_core::MaterialKind`, `StructuralNode::apply_localized_damage` |
| Rooms, floor slabs, interior furnishings | Partial | Each building gets one floor slab and 4 props at fixed offsets. No rooms, per-floor slabs or room-slot layouts. | `TS:src/structure/interior.ts`, `TS:src/world/contents.ts` | `pd_destruction::ensure_site_buildings_spawned_system` |
| Rubble bodies with caps and distance-prioritized cleanup | Partial | 500-body pool, soft threshold 360, sleeping tier, dozer-distance absorption into the pile. The policy wording differs ("recycle oldest" vs the reference's "farthest first, protect near dozer"). In the ranch scenario, rubble does not stay put: rolled fixture remnants and pushed chunks end 80+ units off the footprint, and the blade crushes most chunks into the `PileField` (22.7 mass at tick 300) where they disappear. | `TS:src/sim/debris.ts`, `TS:src/sim/cleanup.test.ts` | `pd_destruction::DebrisPoolRegistry`, `update_debris_lifecycle_system` |
| `PileField` (real heap height and resistance, blade push, compaction) | Partial | Mass-conserving 3×3 kernel. `test_pile_field_mass_conservation_resistance_and_blade_push`. The heap has no visible rendering: ranch captures show no mound where the crushed mass sits. | `TS:src/sim/pile.ts`, `TS:src/sim/corePileResistance.ts` | `pd_world::PileField` |
| Ground marks (chips, scrapes, dust) | Partial | 300-slot `GroundDecalPool` for tread marks and crushed scars. No temporary chip or dust fade-out. | `TS:src/sim/worldSim.ts` (`GroundMark`) | `pd_world::GroundDecalPool`, `process_ground_decal_requests_system` |
| Permanent collapsed-site scar | Done | | `TS:src/sim/worldSim.ts` (`CollapsedSite`) | `pd_world::CollapsedSite` |
| Far-debris retirement (28 u / 8 s, road linger) and demolished-building release | Missing | Debris leaves only through the pool cap. Demolished buildings keep all nodes. | `TS:src/sim/retirement.ts` | — |
| Destructible prop catalog with 7 profiles | Partial | 25 archetypes cover all 7 profiles (`AssetCatalogDef`). The reference has about 50 props with variants, bird gag, sparks and per-lot dressing. | `TS:src/world/catalog.ts`, `TS:src/sim/objectBehavior.ts` | `pd_destruction::PropArchetype::catalog_def`, `apply_prop_damage_and_profile` |
| Explosive chain-depth cap | Done | `MAX_EXPLOSION_GENERATION = 1`. | `TS:src/sim/assets.ts` (`EXPLODE.maxGeneration`) | `pd_destruction::MAX_EXPLOSION_GENERATION` |
| Full-building bonus, paid once | Partial | `full_collapse_bonus = stories × 120`, guarded by `collapse_bonus_paid`. The reference's per-cell cash table (`cellCash.ts`) is not ported, so payouts differ. | `TS:src/structure/cellCash.ts`, `TS:src/sim/worldSim.ts` | `pd_destruction::BuildingGraphEntry` |
| Landmark "90% of original members gone and settled" rule | Missing | A `destroyed_fraction()` helper exists but nothing checks it as an objective. | `TS:src/game/job.ts`, `TS:docs/brick-job.md` | `BuildingGraphEntry::destroyed_fraction` |
| Building, site and construction JSON packages | Missing | Only a 7-entry RON manifest (archetype *names* and max stories) compiled in with `include_str!`. Building geometry is procedural. | `TS:src/world/data/**`, `TS:src/world/archetypes.ts`, `TS:docs/construction-authoring.md` | `assets/prefabs/campaign_levels.prefab.ron` |
| Accelerated 20-minute soak and perf budgets | Missing | No soak test. | `TS:src/sim/soak.test.ts`, `TS:src/sim/bench.test.ts` | — |

## Dozer

| Item | Status | Note | TS | Rust |
| --- | --- | --- | --- | --- |
| Throttle and steer drive (W/S, A/D, arrows) | Done | Momentum, lateral grip, speed-falloff steering, turn scrub. Constants mirror the reference. | `TS:src/vehicle/dozer.ts` | `pd_dozer::compute_skid_steer_kinematics` |
| Powered blade surge (0.72 s, 1.12× cap, 2.15 s cooldown, 0.74× drag) | Done | `test_issue_11_timed_powered_blade_surge_and_5_blade_points`. | `TS:src/vehicle/dozer.ts` | `pd_dozer::DOZER_PUSH_*` |
| 5-point blade contact and blade-slam bonus | Done | | `TS:src/sim/coreImpact.ts` | `pd_dozer::compute_blade_contact_points`, `src/lib.rs` (`slam_bonus`) |
| Chassis pitch and roll spring-damper | Partial | Pitch and roll kicks exist. The reference's 1.0.48–1.0.49 dozer roll, continuous tread scrub and heft-scaled camera shake landed after this port's baseline. | `TS:src/vehicle/dozer.ts`, `TS:src/fx/cameraShake.ts` | `pd_dozer::kick_chassis` |
| Upgrades: blade +42%, engine +28%, push +35% | Done | Same increments and effects (shear, speed/accel/heat cooling, push time/force/track cooling). | `TS:src/game/upgrades.ts` | `pd_dozer::UpgradeMultipliers` |
| Milestones $350 / $900 / $1,600 | Partial | The port adds "every +$900 thereafter". The reference stops after three. The reference counts campaign-total cash; the port counts the level ledger, which resets on restart. *(confirm)* | `TS:src/game/constants.ts`, `TS:src/game/Game.ts` (`maybeOfferUpgrade`) | `pd_dozer::UpgradeProgression::evaluate_cash_milestones` |
| Carry upgrades on advance, restore on retry | Done | `test_issue_13_milestone_upgrades_and_campaign_level_retry_vs_advance`. | `TS:src/game/campaignRun.ts` | `UpgradeProgression::{advance_level, retry_level}` |
| Heat and track failure | Partial | The reference **ends the run** with `ENGINE COOKED` / `TRACK THROWN`. The port enters 0.25× limp mode and recovers below 60% / 55%. Neither death string exists. | `TS:src/game/session.ts` (`sessionFailsOn`), `TS:src/game/Game.ts` | `pd_dozer::compute_skid_steer_kinematics` (`breakdown_factor`) |
| Sandbox immunity | Done | Heat and track capped at 75%. `test_sandbox_breakdown_immunity`. | `TS:src/sim/sandbox.test.ts` | `MechanicalStress::sandbox_mode` |
| Gauge warnings (release-blade, back-off-rubble prompts) | Missing | | `TS:src/sim/dozerCues.ts`, `TS:src/render/instrumentValues.ts` | — |
| Anti-wedging depenetration watchdog | Different-by-design | Needed because Avian2D is a rigid-body solver. The reference has no physics engine. | — | `pd_dozer::compute_skid_steer_kinematics`, `test_watchdog_uses_true_collider_overlap_not_broad_hit_radius` |
| Per-track keys Q/Z and E/C | Different-by-design *(confirm)* | Port-only. The reference has throttle and steer only. | `TS:src/game/input.ts` | `pd_dozer::read_dozer_keyboard_input_system` |
| Extra blade keys Shift / R / F | Different-by-design *(confirm)* | Port-only. In the reference **R** is restart, so the port's R blade binding conflicts with reference muscle memory. | `TS:src/game/input.ts` | `read_dozer_keyboard_input_system` |

## Town generation

| Item | Status | Note | TS | Rust |
| --- | --- | --- | --- | --- |
| Seeded generation (`seed`, New Layout rerolls) | Partial | `pd_core::SiteSeed` (from `SiteSeedOverride` or a per-level default) and `SplitMix64` exist and drive season and weather. Layout generation is not seeded yet, and New Layout does not exist. | `TS:src/game/rng.ts`, `TS:src/game/session.ts` (`nextSeed`) | — |
| Terrain-first surface grid (grass, scrub, field, wet-edge, water, forest) | Missing | The ground is a flat tilemap of a few tile kinds. | `TS:src/world/terrain.ts`, `TS:src/world/terrainFeatures.ts` | `pd_world::ensure_site_ground_spawned_system` |
| Road graph (nodes, segments, lanes, junction normalization, mesh) | Missing | `RoadSplineGraph` is a fixed 8-waypoint rectangle sized by level index. | `TS:src/world/roads.ts`, `TS:src/world/rural.ts` | `pd_world::RoadSplineGraph::rebuild_for_level` |
| Road auto-tiling | Partial | `compute_road_bitmask` and atlas indices exist. There is no real graph to tile. | `TS:src/render/terrainTiles.ts` | `pd_world::compute_road_bitmask` |
| Parcels, frontage, setbacks, driveways | Missing | | `TS:src/world/parcels.ts`, `TS:src/world/drivewayStyle.ts` | — |
| Archetype / zoning selection per lot | Missing | Each level hard-codes 3 buildings at `(-160,120)`, `(160,120)`, `(0,220)`. | `TS:src/world/archetypes.ts`, `TS:src/world/districts.ts` | `ensure_site_buildings_spawned_system` |
| Urban bands and city composition quotas | Missing | | `TS:src/world/urbanGeography.ts`, `TS:src/world/campaignComposition.ts`, `TS:docs/city-map-fabric.md` | — |
| Lot dressing and lot-use profiles | Missing | 9 street props at fixed positions. | `TS:src/world/dressing.ts`, `TS:src/world/lotUse.ts` | `ensure_site_buildings_spawned_system` (`exterior_specs`) |
| Biomes and ground condition (snow) | Missing | | `TS:src/world/biomes.ts`, `TS:src/world/groundCondition.ts` | — |
| Seasons (Spring/Summer/Autumn/Winter/Random) | Partial | Palette and weather particles per season work. Random resolves from `SiteSeed`, so it stays the same across restarts of a site, as in the reference. Winter has no frozen water because there is no water. | `TS:src/world/season.ts`, `TS:src/fx/seasonWeather.ts` | `pd_core::Season::resolve`, `pd_world::SeasonalPalette` |
| Weather On / Reduced / Off | Partial | On and Off only. | `TS:src/fx/seasonWeather.ts` | `pd_core::WeatherSetting` |
| Town validation (containment, corridors, reachability) | Missing | | `TS:src/world/districts.ts` (`validateTown`) | — |
| Neighbourhood debug overlay (`G`, `?nhood=1`) | Missing | | `TS:src/render/nhoodOverlay.ts` | — |

## Campaign / Time Challenge

| Item | Status | Note | TS | Rust |
| --- | --- | --- | --- | --- |
| Seven levels: County … Governor's Mansion | Partial | Names and order match. Per-level settings (building count, topology, parcel sizes, dressing budget) are reduced to a story count. | `TS:src/game/campaign.ts` | `pd_core::CampaignLevel`, `assets/prefabs/campaign_levels.prefab.ron` |
| County clock countdown per level (3:00–4:00) | Missing | Only an `ELAPSED` count-up clock. | `TS:src/game/campaign.ts` (`timeLimit`) | `pd_core::ElapsedClock` |
| Landmark objective (named building, %, **DOWN**, gold pin and rim arrow) | Missing | Port archetype names are invented ("Rural Farmhouse", "Glass Curtain Skyscraper") and do not match the reference landmarks. | `TS:src/game/campaignRun.ts`, `TS:docs/campaign.md` | — |
| Level dollar target and per-level vs campaign cash | Missing | One ledger, reset on every restart. | `TS:src/game/campaignValue.ts` | `pd_core::CashLedger` |
| Briefing → Begin Level | Missing | | `TS:src/render/permitIntro.ts` | — |
| Results: LEVEL CLEARED / Next Level, failure / Retry Level, COUNTY CLOSED / New Campaign | Missing | Tab cycles to the next level from anywhere and wraps 7/7 → 1/7. | `TS:src/game/Game.ts`, `TS:src/game/sessionTransition.ts` | `pd_ui::update_industrial_hud_system` (Tab) |
| Permit resubmission fee (once per level) | Missing | | `TS:src/game/resubmission.ts` | — |
| Brick Demolition job | Missing | Introductory contract with a 90% rule and $1,200 reward. | `TS:src/game/job.ts`, `TS:docs/brick-job.md` | — |
| Opt-in `campaign` weights on buildings, sites and props | Missing | Depends on building packages. | `TS:src/world/campaignPlacement.ts` | — |

## Sandbox

| Item | Status | Note | TS | Rust |
| --- | --- | --- | --- | --- |
| Sandbox mode on the same 7 levels, no clock or failures | Partial | The mode exists and the levels are shared. There is no clock to disable yet. | `TS:src/game/session.ts` | `pd_core::GameMode::Sandbox` |
| Upgrade hotkeys 1/2/3 without pausing | Done | Picks queue in `UpgradePickQueue` and apply at the next tick boundary. | `TS:src/game/Game.ts` | `pd_dozer::{read_upgrade_hotkeys_system, apply_upgrade_picks_system}` |
| R restart same layout, N new layout | Partial | **B** restarts. There is no seed, so "same layout" is trivially true and "new layout" is impossible. Tab changes level instead. | `TS:src/game/Game.ts` (`handleRunKeys`) | `pd_ui::update_industrial_hud_system` |
| Season picker incl. Random | Partial | See Seasons above. | `TS:src/game/sessionMenu.test.ts` | `pd_ui` New Game modal |
| Ranch benchmark (`?sandbox=1&ranch=1`) | Partial | `SiteTestMap::Ranch`, hand-built from `ranch/main.building.json` (5×3 cells, one floor, timber, gable along x, three fixtures), with the dozer staged as in `ranchLever.ts`. `scenarios/ranch.ron` replays the reference test's input timeline. Gaps: no chimney or door opening, none of `ranchLever.ts`'s judged checks, and the ranch comes down in about 1 s of contact (much faster than the reference). The building-JSON pipeline stays on the roadmap. | `TS:src/debug/ranchLever.ts`, `TS:src/game/Game.test.ts` | `pd_destruction::spawn_ranch_benchmark_site`, `scenarios/ranch.ron` |
| Asset Test Yard and focused asset tests | Missing | | `TS:src/world/testYard.ts`, `TS:docs/asset-test-yard.md` | — |
| `V` spawns a test road vehicle on a generated route | Missing | Ambient traffic follows the fixed loop instead. | `TS:src/vehicle/roadVehicle.ts`, `TS:src/world/routing.ts` | `pd_ai::ensure_site_crowd_and_traffic_spawned_system` |

## UI and menus

| Item | Status | Note | TS | Rust |
| --- | --- | --- | --- | --- |
| Title screen | Partial | Has the permit-application title modal with PLAY and New Game. The reference title is `PERMIT DENIED` with the tagline, permit logo and Reload Latest Build. | `TS:src/render/permitLogo.ts`, `TS:src/render/operatorMenu.ts` | `pd_ui::ensure_industrial_hud_spawned_system` |
| Play / New Game flow (mode, site, layout) | Partial | Cycling buttons for Mode, Level, Season and Weather. The reference picks Timed Challenge vs Sandbox, then site, then standard or randomized layout. | `TS:src/render/operatorMenu.ts`, `TS:src/game/sessionMenu.test.ts` | `pd_ui` (`NewGameModalNode`) |
| Pause menu: Resume, Restart Site, New Game, Controls, Sound, About, Main Menu | Partial | All buttons exist. Controls, Sound and About only set a one-line banner. About lacks package version, publisher contact and third-party notices. | `TS:src/render/operatorMenu.ts`, `TS:src/render/thirdPartyNotices.txt` | `pd_ui::handle_hud_button_interactions_system` |
| Top bar: cash wheels, clock, level / target / landmark line | Partial | 6-digit odometer and clock exist. No campaign line. The clock counts up, not down. | `TS:src/render/hud.ts`, `TS:src/render/cashCounter.ts`, `TS:src/render/equipmentClock.ts` | `pd_ui::update_industrial_hud_system` |
| Operator console: heat and track dials, multipliers, BLADE UP / DOWN | Partial | Text-glyph gauges, not drawn dials. Blade LED colours match. | `TS:src/render/hud.ts`, `TS:src/render/instrumentValues.ts` | `pd_ui` |
| Floating cash popups | Partial | Coalescing pool (50 cap, 100 ms, 2–3 m) is Done. They render as one text banner, not world-anchored popups. | `TS:src/render/cashPopups.ts` | `pd_core::try_award_demolition_cash`, `pd_ui` (`FloatingCashPopupBannerNode`) |
| Upgrade milestone modal | Done | | `TS:src/render/hud.ts` | `pd_ui` (`UpgradeSelectionModalNode`) |
| Debug binder (Inspector / Test Scenarios / Assets, Freeze, Step One Frame) | Missing | The port has a one-line F3 telemetry overlay, F4 collider outlines and F9 / F10 record / replay hotkeys, but no binder. `SimFreeze` is the hook for Freeze / Step One Frame. | `TS:src/debug/binderState.ts`, `TS:docs/debug-binder.md` | `pd_ui` (`DebugOverlayPanelNode`) |
| Perf overlay (`` ` `` / `?perf=1`: frame, sim, debris budgets) | Partial | F3 shows debris, rubble, popup, node and BFS counts. No frame or sim timing. | `TS:src/debug/perf.ts`, `TS:src/render/debugOverlay.ts` | `pd_core::DebugTelemetry` |
| Whole-town overview camera (`G`) | Missing | | `TS:src/game/overviewFrame.ts` | `src/lib.rs::camera_follow_and_cutaway_system` |
| Interior cutaway and occlusion fade | Partial | A per-building roof-alpha fade near the dozer, which also drops the whole roof to 15% alpha once the exterior is breached. The reference keeps the roof on a breach (`ranchLever.ts` check `roof-stays-on-breach`) and fades per wall, per bay and per upper-floor tile, so the port hides roof sag and hinge. | `TS:src/render/occlusion.ts`, `TS:src/render/visibility.test.ts` | `pd_world::CutawayFootprint` |
| Terms / privacy gating, analytics consent | Different-by-design | Web-only. Revisit if the port ships on the web. | `TS:src/privacy/` | — |

## Audio

| Item | Status | Note | TS | Rust |
| --- | --- | --- | --- | --- |
| Synthesized SFX (no audio files) | Partial | Five one-shot WAVs generated at startup (hiss, crunch, rumble, blast, ding). The reference synthesizes with WebAudio oscillators. | `TS:src/audio/synth.ts` | `src/lib.rs::synthesize_wav_sfx_bytes` |
| Continuous engine drone tied to throttle, grind loop, ambient loop | Missing | `AudioSfxKind::DieselRev` maps to the hiss one-shot. | `TS:src/audio/synth.ts` (`AudioBus`) | `play_synthesized_audio_events_system` |
| Mute (`M` key and menu) | Partial | Menu Sound toggle only, no `M` key. Every button press plays the cash ding, regardless of the button. | `TS:src/game/Game.ts` (`handleFrameKeys`) | `pd_ui::handle_hud_button_interactions_system` |
| Hush while paused or menus open | Missing | | `TS:src/game/Game.ts` (`advance`) | — |

## Input and touch

| Item | Status | Note | TS | Rust |
| --- | --- | --- | --- | --- |
| Keyboard driving | Done | | `TS:src/game/input.ts` | `pd_dozer::read_dozer_keyboard_input_system` |
| Release held input on blur, menus, rotation, backgrounding | Missing | | `TS:src/game/input.ts` (`onBlur`), `TS:src/game/Game.ts` (`releaseControls`) | — |
| Menu keyboard focus containment | Missing | Enter starts a game from Title or New Game. B and Tab are read in every state. | `TS:src/render/operatorMenu.ts` | `pd_ui::update_industrial_hud_system` |
| Touch controls (stick with 15% dead zone, POWER BLADE button) | Missing | | `TS:src/render/touchControls.ts`, `TS:src/game/input.ts` (`stickAxes`) | — |
| Gamepad | Different-by-design *(confirm)* | Not in the reference. Cheap in Bevy, and a natural desktop counterpart to touch. `leafwing-input-manager` is already a dependency but unused. | — | `crates/pd_dozer/Cargo.toml` |

## Replay and testing

| Item | Status | Note | TS | Rust |
| --- | --- | --- | --- | --- |
| Recorded playthroughs (record, replay, MATCH / DRIFT digest) | Done | `permit-denied-scenario` v1 (RON) mirrors `permit-denied-playthrough` v1: setup, change-only `(tick, left, right, blade)` controls, tick-stamped upgrade actions and an end-state digest, plus captures, overlays and an exit tick. F9 records from a site restart into `scenarios/rec-*.ron`, and any restart also stops a recording. F10 replays with live input ignored and holds on MATCH / DRIFT. Resubmission-fee and road-car actions are not recorded because the port has neither feature. There is no Pause / Restart badge control mid-replay. | `TS:src/debug/playthrough.ts`, `TS:src/game/playthroughRun.ts`, `TS:docs/testing.md#recorded-playthroughs` | `pd_replay`, `src/replay_hud.rs` |
| Replay regression test (ranch demolition replays to MATCH) | Done | Records the ranch from live input, then replays it in place (with live keys hammered) and fresh, to MATCH. | `TS:src/game/Game.test.ts` | `tests/ranch.rs`, `tests/replay.rs`, `tests/replay_hotkeys.rs` |
| Scenario launch links (`?level=`, `?seed=`, `?sandbox=1`, `?ranch=1`, `?mode=challenge`) | Partial | `cargo run -- --scenario <file.ron> [--out <dir>] [--headless] [--write-expect]`. The scenario file carries mode, level, seed, season, weather and test map. There are no per-setting flags and no Time Challenge mode. | `TS:src/game/session.ts` (`parseSessionFromSearch`) | `src/scenario_cli.rs` |
| Debug automation bridge (`window.__pd`) | Partial | Scenario files are the scripted control surface (inputs, captures, exit, verdict). There is no live bridge for pose edits or queries. | `TS:src/debug/bridge.ts` | `pd_replay::{begin_replay, begin_recording, finish_recording}` |
| Screenshot and asset capture | Partial | Scenario capture ticks write PNG + telemetry JSON, a labeled `contact-sheet.png` and `summary.json`, with optional collider overlay and zoom. No title, catalog, terrain or season capture suites. | `TS:scripts/capture/harness.mjs`, `TS:src/debug/assetCapture.ts` | `src/scenario_cli.rs`, `src/contact_sheet.rs` |
| Unit tests on core functions with extreme inputs (Gate 2) | Done | NaN / ∞ / dt extremes / `u32::MAX` cash across crates, plus hostile scenario files. | — | `src/lib.rs::tests::test_kinematic_and_ledger_extremes`, per-crate `tests` |
| Headless restart idempotence (Gate 3) | Done | | `TS:src/game/sessionTransition.test.ts` | `src/lib.rs::tests::test_idempotent_site_restart` |
| Test tiers (unit / integration / soak / bench) | Partial | Everything runs in one `cargo test`. No integration matrix across levels × seeds, no soak, no benches. | `TS:docs/testing.md`, `TS:vite.config.ts` | — |
| Generation matrices (every level × seeds builds a valid town) | Missing | Depends on town generation. | `TS:src/world/campaignLayout.test.ts`, `TS:src/world/generationSmoke.test.ts` | — |

## Tooling and CI

| Item | Status | Note | TS | Rust |
| --- | --- | --- | --- | --- |
| CI on every PR (Linux + Windows) | Missing | No `.github/` at all. Gates 1–3 run only locally. | `TS:.github/workflows/ci.yml` | — |
| Portable build config | Partial | `.cargo/config.toml` pins the `x86_64-pc-windows-gnu` linker to a user-specific WinGet path. It breaks GNU-toolchain builds on any other machine. MSVC is unaffected. | — | `.cargo/config.toml` |
| Lint gate | Done | Workspace clippy denies `unwrap`, `expect`, `panic`, `indexing_slicing`, `float_cmp`, and the rest. | `TS:.fallowrc.jsonc` (dead code, dupes, complexity) | `Cargo.toml` `[workspace.lints.clippy]` |
| Dead-dependency and dead-code check | Missing | `leafwing-input-manager` is declared but unused. Consider `cargo machete` / `cargo udeps` as a fallow analogue. | `TS:scripts/fallow-health.mjs` | `crates/pd_dozer/Cargo.toml` |
| Repo freshness preflight and SessionStart hook | Missing | See "Skills and rules worth adapting". | `TS:scripts/repo-preflight.mjs`, `TS:.claude/hooks/session-start.mjs` | — |
| Shell guard hook (block staging build output, ask on force-push) | Missing | | `TS:.claude/hooks/shell-guard.mjs` | — |
| Release / versioning skill | Missing | Port is `0.1.0` with no release flow. | `TS:.claude/skills/release/SKILL.md` | `Cargo.toml` |
| PR template, issue templates, CONTRIBUTING | Missing | | `TS:.github/` | — |

## Platform and web

| Item | Status | Note | TS | Rust |
| --- | --- | --- | --- | --- |
| Native desktop binary | Different-by-design | The port is a native Bevy app. The reference is browser-only. | — | `src/main.rs` |
| Web build (wasm32) | Missing | `fit_canvas_to_parent` is set but no wasm target, `index.html` or bundler (trunk / wasm-bindgen) exists. Avian2D and Bevy both support wasm. | `TS:vite.config.ts`, `TS:index.html` | `src/main.rs::main` |
| PWA install, offline shell, update prompt | Different-by-design | Only relevant if the port ships on the web. | `TS:src/pwa/`, `TS:scripts/pwa.mjs` | — |
| Hosting and deploy (Vercel, Cloudflare R2 catalog) | Different-by-design | Out of scope until a distribution target is chosen. | `TS:scripts/deploy-vercel.mjs` | — |
| Catalog and designer entries (`/catalog/`, `/designer/`) | Different-by-design | Separate web tools. They can keep running against the reference's JSON packages. | `TS:src/catalog/`, `TS:src/designer/` | — |
| Mobile layout (phone, tablet, landscape) | Missing | Depends on touch and a web or mobile target. | `TS:scripts/verify-mobile.mjs` | — |

## Port-only features (not in the reference)

These exist only in the port. Each needs a keep-or-cut decision so parity work doesn't fight it.

| Item | Status | Note | Rust |
| --- | --- | --- | --- |
| Panicking pedestrian crowds | Different-by-design *(confirm)* | The reference has no pedestrians. | `pd_ai::fixed_crowd_and_traffic_ai_system`, `safe_flee_direction` |
| Police cruiser, SWAT van, county inspector traffic | Different-by-design *(confirm)* | The reference has a police **routing contract only** and no police AI. Its copy rules forbid "wanted stars"-style escalation, so keep these as ambient traffic only. | `pd_ai::VehicleArchetype` |
| Spline-to-physics vehicle knockback and oil slicks | Different-by-design *(confirm)* | The reference's road vehicles use modular damage regions and attachments, not rigid-body knockback. | `pd_ai::fixed_crowd_and_traffic_ai_system` |
| Recurring +$900 milestones | Different-by-design *(confirm)* | See Dozer. | `pd_dozer::SUBSEQUENT_MILESTONE_STEP` |
| Heat / track limp mode instead of run end | Different-by-design *(confirm)* | See Dozer. In the port this is a Partial gap until decided. | `pd_dozer::compute_skid_steer_kinematics` |

---

## Skills and rules worth adapting from the reference

From `TS:CLAUDE.md`, `TS:.claude/skills/` and `TS:.claude/hooks/`:

| Source | Adapt? | How |
| --- | --- | --- |
| Copy rules (`BLADE UP` / `BLADE DOWN`, `ENGINE COOKED` / `TRACK THROWN` / `COUNTY CLOCK`, banned words) | **Adopted** | Now in CLAUDE.md under "Reference implementation". |
| "Stay on the lot" scope lock (no second town, no metagame or unlocks, seven-level campaign in scope) | Yes | Add to CLAUDE.md as a scope rule. It decides several *(confirm)* rows above. |
| "Simulation in world coords; render never mutates sim state" | Yes | Already true in practice. Make it a Rule B bullet so `Update` render systems never write sim components. |
| "Same seed + same inputs reproduce a run" | **Adopted** | Rule B "Deterministic simulation" in CLAUDE.md, gated by `tests/determinism.rs` and the replay MATCH tests. |
| Test tiers (unit presubmit vs integration, soak, bench) | Yes | Mark slow tests `#[ignore]` or put them behind a cargo feature, and run them in CI only. This keeps `cargo test` a fast presubmit. |
| `stacked-pr` skill | Yes, nearly verbatim | Language-agnostic. Swap `npm test` / `npm run check` for the three Rule E gates and drop the Vercel preview section. |
| `release` skill | Later | Port once CI and a distribution target exist. Bump `[workspace.package] version` instead of `package.json`. |
| `repo:preflight` + `SessionStart` hook | Yes | Port `scripts/repo-preflight.mjs` logic (fetch, fast-forward only when provably safe, report `FRESHNESS UNVERIFIED` otherwise). It can stay a Node script or become a small Rust `xtask`. Have the hook inject the cargo gate commands. |
| `shell-guard` PreToolUse hook | Yes | Deny staging `target/` or `*.exe`, ask on force-push. The parser handles PowerShell here-strings, which matters on this Windows box. |
| `grilling` / `grill-me` | Optional | MIT-licensed (Matt Pocock). Copy with its `LICENSE` and `ATTRIBUTION.md` if wanted. |
| `grill-with-ui` | No | Node server and browser page. Low value here. |
| Working rules ("don't commit unless asked", "never commit build output", "deliver a playable slice, not a plan") | Yes | Short CLAUDE.md bullets. |

---

## README claims that aren't true yet

Checked against code and assets at `eca708d`. Claims not listed here held up.

1. **Asset folders don't exist.** The README tree lists `assets/atlases/`, `assets/tiles/`, `assets/audio/` and `assets/fonts/`. Git tracks only `assets/prefabs/campaign_levels.prefab.ron`. The atlas is generated in code (`pd_world::generate_cohesive_atlas_rgba_bytes`), audio is synthesized (`src/lib.rs::synthesize_wav_sfx_bytes`) and text uses Bevy's default font. No 7-segment or stencil font is loaded.
2. **`assets/prefabs/` does not hold "building definitions & structural node graphs".** It holds one level manifest with archetype names, max stories and a yield impulse, compiled in with `include_str!`. Building graphs are procedural (`build_procedural_building_graph`). `foundation_yield_impulse` is parsed but nothing reads it.
3. **Title text.** The README quotes "COUNTY BUILDING DEPARTMENT **—** APPLICATION FOR DEMOLITION". The UI uses a hyphen (`pd_ui`). Cosmetic.
4. **"Cycle Next Campaign Level (`1/7` – `7/7`)" with Tab.** Tab wraps from 7/7 back to 1/7 and goes through the *advance* path, so upgrades carry into County. Tab and **B** are also read on the Title and New Game screens and in Sandbox, not only during a campaign.
5. **Repo name.** The README (and CLAUDE.md) call the root `permit-denied-bevy`. The GitHub repo is `permit-denied-rust-edition` and the Cargo package is `permit_denied`. The binary path claim (`target/release/permit_denied(.exe)`) is correct.
6. **"Only the standard Rust toolchain" / Windows via `rustup-init` with MSVC build tools.** True for MSVC. `.cargo/config.toml` hard-codes a GNU linker under `C:/Users/viral/AppData/...`, so anyone else on the `windows-gnu` toolchain (which is this machine's default) gets a linker error.
7. **"Runs smoothly at 60 FPS out of the box."** Not verified. There is no frame-time measurement or benchmark in the repo, and the F3 overlay shows no timing. The fixed simulation step is now 60 Hz (it was 64 Hz at the audit).

Verified true: Enter / PLAY start, the New Game options, every key in the controls table except the Tab caveats, BLADE UP green / BLADE DOWN amber, the surge numbers (0.72 s, 1.12×, 2.15 s, 0.74×), the blade-slam bonus, crushing sleeping rubble into scars, cash for furniture, propane, hydrants and vehicles, the milestone amounts and modal, the upgrade effects, retry vs advance upgrade snapshots, limp mode with recovery thresholds, Sandbox immunity, the crate list and DAG, async BFS, the 500-cap 3-tier pool, spline-to-physics traffic, the dev-profile `opt-level = 3` for dependencies, and Gates 1–3 as commands. `cargo test --workspace` passed all 35 tests during this audit. Clippy (Gate 1) was not run.

---

## Recommended order of work

1. **Replay and capture first.** Done on `feat/record-replay`: `SiteSeed`, 60 Hz, fixed-latency BFS, F9 / F10 record and replay, the `--scenario` CLI with captures and contact sheets, the ranch benchmark, and MATCH tests. Avian2D `enhanced-determinism` was deliberately left off; revisit with Linux CI.
2. **CI and hygiene.** Add a GitHub Actions workflow running Gates 1–3 on Linux and Windows, plus a wasm `cargo check` once step 9 starts. Move the machine-specific linker out of `.cargo/config.toml`. Remove or use `leafwing-input-manager`. Port the preflight and shell-guard hooks and the `stacked-pr` skill. Fix the README items above.
3. **Decide the *(confirm)* rows** (pedestrians, police/SWAT, Q/Z/E/C, B/Tab vs R/N, limp vs death, recurring milestones). This is a short decision pass, so steps 4–6 don't build on choices that later get reversed.
4. **Time Challenge loop.** County clock countdown with per-level time limits, dollar targets, level vs campaign cash, `ENGINE COOKED` / `TRACK THROWN` / `COUNTY CLOCK` endings, results modals (LEVEL CLEARED / Retry Level / COUNTY CLOSED), R / N parity and permit resubmission. Use a stand-in landmark (the tallest spawned building) with the 90% settled rule until real packages arrive.
5. **Building data pipeline.** Load the reference's `*.building.json`, `*.layout.json` and `*.construction.json` with serde: copy them in, or point at the reference read-only during development. Compile them into `StructuralNode` graphs with rooms, slabs, materials, cladding and roof styles. Port `cellCash.ts` so payouts match. Then rebuild the hand-built ranch (`spawn_ranch_benchmark_site`) from its package, with the chimney and the door opening.
6. **Town generation.** Terrain grid, then road graph, then parcels, then archetype selection, then dressing. Then campaign placement weights, urban bands, city composition, biomes and frozen water. Add per-level × seed generation matrices as an integration tier.
7. **Simulation fidelity.** Staged sag, crack and fall with cross-building chain collapse, debris and building retirement, more roof types, prop catalog expansion to the reference roster, `V` road vehicles from the 12 vehicle definitions, gauge-warning cues, and a soak test.
8. **UI, menus and audio.** Menu flow parity (Play, then mode, then site, then layout), a real Controls page, About with version and notices, drawn dials, world-anchored cash popups, landmark pin and arrow, `G` overview, `` ` `` perf overlay with frame and sim timing, and a debug binder subset (Freeze, Step One Frame, Record / Replay buttons). Audio: throttle-driven engine drone, grind and ambient loops, `M` mute, hush on pause.
9. **Input and platform.** Release-on-blur and menu focus containment, then touch controls and gamepad. Then a wasm build if the port is meant to reach the browser, with mobile layout checks after that.
