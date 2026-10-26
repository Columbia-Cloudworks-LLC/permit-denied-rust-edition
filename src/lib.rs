//! `permit_denied`: Game library assembling the 6 defensive Bevy workspace crates
//! (`pd_core`, `pd_world`, `pd_dozer`, `pd_destruction`, `pd_ai`, `pd_ui`), procedural WAV
//! audio synthesis, 3-tier debris heightmap crushing, prop destruction, and Verification Gates.

pub mod contact_sheet;
pub mod overlays;
pub mod replay_hud;
pub mod scenario_cli;

use avian2d::prelude::*;
use bevy::audio::AudioPlayer;
use bevy::prelude::*;
use bevy::tasks::AsyncComputeTaskPool;
use pd_ai::PdAiPlugin;
use pd_core::{
    AppState, AudioSfxKind, AudioTriggerEvent, DebugTelemetry, DecalKind, DecalSpawnRequest,
    DemolitionCashEvent, IsoBoxAssembly, IsoExtrudedBox, ParticleBurstEvent, ParticleBurstKind,
    PdCorePlugin, PdSystemSet, SessionEpoch, SimOrdinal, WorldPose, fill_sim_order, sanitize_f32,
    sanitize_vec2, world_to_iso_screen,
};
use pd_destruction::{
    BuildingGraphRegistry, DebrisChunk, DebrisPoolRegistry, DebrisTier, DestructibleProp,
    MAX_EXPLOSION_GENERATION, ModularBuilding, NodeIndex, PdDestructionPlugin,
    apply_prop_damage_and_profile, build_building_compound_collider,
    build_building_iso_assembly_from_entry, queue_structural_integrity_evaluation,
    spawn_pooled_debris_chunk_with_material,
};
use pd_dozer::{
    BladeState, MechanicalStress, PdDozerPlugin, PlayerVehicle, TrackInput, UpgradeMultipliers,
    apply_dozer_impulse, compute_blade_contact_points,
};
use pd_ui::PdUiPlugin;
use pd_world::{CutawayFootprint, PdWorldPlugin, PileField};

/// Marker component for the isometric 2.5D orthographic camera.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct IsometricCamera;

/// Resource holding procedurally synthesized 16-bit mono PCM WAV audio handles.
#[derive(Resource, Debug, Clone, Default)]
pub struct SynthesizedAudioBank {
    pub hydraulic_hiss: Option<Handle<AudioSource>>,
    pub structural_crunch: Option<Handle<AudioSource>>,
    pub collapse_rumble: Option<Handle<AudioSource>>,
    pub propane_blast: Option<Handle<AudioSource>>,
    pub cash_ding: Option<Handle<AudioSource>>,
}

/// Synthesizes a valid 16-bit mono PCM `.wav` byte buffer in memory so audio works out-of-the-box.
#[must_use]
pub fn synthesize_wav_sfx_bytes(
    base_freq_hz: f32,
    duration_secs: f32,
    noise_mix: f32,
) -> Vec<u8> {
    let sample_rate: u32 = 22_050;
    let safe_dur = sanitize_f32(duration_secs, 0.15).clamp(0.04, 1.0);
    let num_samples = ((sample_rate as f32) * safe_dur) as usize;
    let data_bytes = (num_samples * 2) as u32;
    let file_size_minus_8 = 36_u32.saturating_add(data_bytes);

    let mut buf = Vec::with_capacity(44 + num_samples * 2);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_size_minus_8.to_le_bytes());
    buf.extend_from_slice(b"WAVEfmt ");
    buf.extend_from_slice(&16_u32.to_le_bytes()); // PCM chunk size
    buf.extend_from_slice(&1_u16.to_le_bytes()); // PCM format = 1
    buf.extend_from_slice(&1_u16.to_le_bytes()); // Mono = 1 channel
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // Byte rate
    buf.extend_from_slice(&2_u16.to_le_bytes()); // Block align
    buf.extend_from_slice(&16_u16.to_le_bytes()); // Bits per sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_bytes.to_le_bytes());

    let freq = sanitize_f32(base_freq_hz, 220.0).clamp(40.0, 2_400.0);
    let n_mix = sanitize_f32(noise_mix, 0.2).clamp(0.0, 1.0);
    let mut lfsr: u32 = 0xACE1_u32;

    for i in 0..num_samples {
        let t = (i as f32) / (sample_rate as f32);
        let env = (1.0 - (i as f32) / (num_samples.max(1) as f32)).powi(2);
        let tone = (t * freq * std::f32::consts::TAU).sin();

        // Deterministic LFSR noise for hydraulic hiss / crunch / rumble
        lfsr ^= lfsr << 13;
        lfsr ^= lfsr >> 17;
        lfsr ^= lfsr << 5;
        let noise = ((lfsr % 2000) as f32 / 1000.0) - 1.0;

        let mixed = (tone * (1.0 - n_mix) + noise * n_mix) * env * 0.35;
        let pcm = (sanitize_f32(mixed, 0.0).clamp(-1.0, 1.0) * 32_000.0) as i16;
        buf.extend_from_slice(&pcm.to_le_bytes());
    }

    buf
}

fn init_synthesized_audio_bank_system(
    mut bank: ResMut<SynthesizedAudioBank>,
    audio_assets: Option<ResMut<Assets<AudioSource>>>,
) {
    if bank.structural_crunch.is_some() {
        return;
    }
    let Some(mut audio_assets) = audio_assets else {
        return;
    };

    let make_source = |freq: f32, dur: f32, noise: f32| -> AudioSource {
        AudioSource {
            bytes: synthesize_wav_sfx_bytes(freq, dur, noise).into(),
        }
    };

    bank.hydraulic_hiss = Some(audio_assets.add(make_source(420.0, 0.14, 0.75)));
    bank.structural_crunch = Some(audio_assets.add(make_source(110.0, 0.22, 0.65)));
    bank.collapse_rumble = Some(audio_assets.add(make_source(65.0, 0.45, 0.80)));
    bank.propane_blast = Some(audio_assets.add(make_source(85.0, 0.38, 0.90)));
    bank.cash_ding = Some(audio_assets.add(make_source(880.0, 0.10, 0.05)));
}

fn play_synthesized_audio_events_system(
    mut commands: Commands,
    telemetry: Res<DebugTelemetry>,
    bank: Res<SynthesizedAudioBank>,
    mut audio_events: EventReader<AudioTriggerEvent>,
) {
    if !telemetry.sound_enabled {
        audio_events.clear();
        return;
    }

    for ev in audio_events.read() {
        let maybe_handle = match ev.kind {
            AudioSfxKind::BladeHydraulicHiss | AudioSfxKind::DieselRev => &bank.hydraulic_hiss,
            AudioSfxKind::StructuralCrunch => &bank.structural_crunch,
            AudioSfxKind::CollapseRumble => &bank.collapse_rumble,
            AudioSfxKind::PropaneBlast => &bank.propane_blast,
            AudioSfxKind::CashDing => &bank.cash_ding,
        };
        if let Some(handle) = maybe_handle {
            commands.spawn((
                AudioPlayer::new(handle.clone()),
                PlaybackSettings::DESPAWN,
            ));
        }
    }
}

fn setup_isometric_camera_system(
    mut commands: Commands,
    existing_cam: Query<Entity, With<IsometricCamera>>,
) {
    if existing_cam.iter().next().is_some() {
        return;
    }

    commands.spawn((
        Name::new("Isometric 2.5D Orthographic Camera"),
        IsometricCamera,
        Camera2d,
        Transform::from_xyz(0.0, 0.0, 900.0),
        GlobalTransform::default(),
    ));
    info!("Isometric 2.5D camera initialized; entering main loop.");
}

/// Smoothly follows the armored bulldozer in isometric screen space while evaluating building
/// interior cutaway opacity in Cartesian `WorldPose` space.
fn camera_follow_and_cutaway_system(
    time: Res<Time>,
    dozer_query: Query<(Option<&WorldPose>, &Transform), (With<PlayerVehicle>, Without<IsometricCamera>)>,
    mut cam_query: Query<&mut Transform, (With<IsometricCamera>, Without<PlayerVehicle>)>,
    mut cutaways: Query<
        (
            &mut CutawayFootprint,
            Option<&mut Sprite>,
            Option<&mut IsoExtrudedBox>,
            Option<&mut IsoBoxAssembly>,
        ),
        (Without<PlayerVehicle>, Without<IsometricCamera>),
    >,
) {
    let dt = sanitize_f32(time.delta_secs(), 0.016).clamp(0.0001, 0.05);
    let Ok((dozer_pose, dozer_tf)) = dozer_query.get_single() else {
        return;
    };
    let dozer_world_xy = dozer_pose.map_or_else(
        || sanitize_vec2(dozer_tf.translation.truncate(), Vec2::ZERO),
        |p| sanitize_vec2(p.pos, Vec2::ZERO),
    );
    let dozer_screen_xy = dozer_pose.map_or_else(
        || sanitize_vec2(dozer_tf.translation.truncate(), Vec2::ZERO),
        |p| world_to_iso_screen(p.pos, p.elev_z),
    );

    if let Ok(mut cam_tf) = cam_query.get_single_mut() {
        let current_xy = sanitize_vec2(cam_tf.translation.truncate(), Vec2::ZERO);
        let next_xy = current_xy + (dozer_screen_xy - current_xy) * (6.0 * dt).clamp(0.0, 1.0);
        cam_tf.translation.x = sanitize_f32(next_xy.x, current_xy.x);
        cam_tf.translation.y = sanitize_f32(next_xy.y, current_xy.y);
    }

    for (mut footprint, sprite, base_box, assembly) in &mut cutaways {
        footprint.update_alpha(dozer_world_xy, dt);
        if let Some(mut sp) = sprite {
            sp.color = sp.color.with_alpha(footprint.current_roof_alpha);
        }
        if let Some(mut b_box) = base_box {
            b_box.alpha = footprint.current_roof_alpha;
        }
        if let Some(mut asm) = assembly {
            asm.alpha_multiplier = footprint.current_roof_alpha;
        }
    }
}

/// Pre-allocated scratch buffers for `evaluate_dozer_structural_impacts_system` (Zero Heap Churn).
#[derive(Default)]
pub struct ImpactScratch {
    pub destroyed_ids: Vec<NodeIndex>,
    pub candidate_cells: Vec<(NodeIndex, f32)>,
    pub blast_queue: Vec<(Vec2, f32, f32, u8)>,
    /// Stable `SimOrdinal` visit orders (query order depends on archetype history).
    pub building_order: Vec<(u64, Entity)>,
    pub prop_order: Vec<(u64, Entity)>,
    pub rubble_order: Vec<(u64, Entity)>,
}

/// Evaluates dozer blade impacts against modular buildings (spatial per-cell wall breaching with
/// material HP & cladding), interior/exterior props (7 data-driven destruction profiles & chained
/// explosions), and `PileField` / sleeping rubble mass conservation (`Epic #3`).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn evaluate_dozer_structural_impacts_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    blade_state: Res<BladeState>,
    upgrades: Res<UpgradeMultipliers>,
    (mut track_input, mut stress, mut graph_registry, mut debris_pool, mut pile_field): (
        ResMut<TrackInput>,
        ResMut<MechanicalStress>,
        ResMut<BuildingGraphRegistry>,
        ResMut<DebrisPoolRegistry>,
        ResMut<PileField>,
    ),
    (mut cash_events, mut decal_events, mut particle_events, mut audio_events): (
        EventWriter<DemolitionCashEvent>,
        EventWriter<DecalSpawnRequest>,
        EventWriter<ParticleBurstEvent>,
        EventWriter<AudioTriggerEvent>,
    ),
    mut scratch: Local<ImpactScratch>,
    ordinals: Query<&SimOrdinal>,
    mut dozer_query: Query<
        (
            &mut PlayerVehicle,
            Option<&WorldPose>,
            &Transform,
            Option<&Collider>,
        ),
        With<PlayerVehicle>,
    >,
    mut building_query: Query<
        (
            Entity,
            Option<&WorldPose>,
            &Transform,
            Option<&Collider>,
            &ModularBuilding,
            &mut CutawayFootprint,
            Option<&mut IsoExtrudedBox>,
            Option<&mut IsoBoxAssembly>,
        ),
        (With<ModularBuilding>, Without<PlayerVehicle>),
    >,
    mut prop_query: Query<
        (
            Entity,
            Option<&mut WorldPose>,
            &Transform,
            &mut DestructibleProp,
            Option<&mut IsoExtrudedBox>,
        ),
        (Without<PlayerVehicle>, Without<ModularBuilding>),
    >,
    mut sleeping_rubble_query: Query<
        (Entity, Option<&WorldPose>, &Transform, &mut DebrisChunk),
        (
            Without<PlayerVehicle>,
            Without<ModularBuilding>,
            Without<DestructibleProp>,
        ),
    >,
) {
    let Ok((mut dozer_veh, dozer_pose, dozer_tf, dozer_collider)) = dozer_query.get_single_mut()
    else {
        return;
    };
    let dozer_xy = dozer_pose.map_or_else(
        || sanitize_vec2(dozer_tf.translation.truncate(), Vec2::ZERO),
        |p| sanitize_vec2(p.pos, Vec2::ZERO),
    );
    debris_pool.dozer_world_pos = dozer_xy;
    let dozer_heading = dozer_pose.map_or_else(
        || sanitize_f32(dozer_tf.rotation.to_euler(EulerRot::ZYX).0, 0.0),
        |p| sanitize_f32(p.heading_rad, 0.0),
    );
    let dozer_rot = Rotation::radians(dozer_heading);
    let forward_dir = Vec2::new(dozer_heading.cos(), dozer_heading.sin());
    let dozer_speed = Vec2::new(dozer_veh.vx, dozer_veh.vy).length();
    let blade_down = *blade_state == BladeState::BladeDown;
    let blade_points = compute_blade_contact_points(dozer_xy, dozer_heading, blade_down);
    let safe_upgrades = upgrades.sanitized();
    let push_force_mul = dozer_veh
        .pushing_force_multiplier
        .max(safe_upgrades.aux_ram)
        .clamp(1.0, pd_dozer::MAX_SAFE_MULTIPLIER);
    let slam_bonus = if dozer_veh.blade_landed_this_tick {
        10.0
    } else {
        0.0
    };
    let task_pool = AsyncComputeTaskPool::get();

    // Sample PileField drag/resistance under the dozer and push/compact heaps (`Issue #16`)
    let pile_res = pile_field.pile_resistance_at(dozer_xy);
    let mut contact_load = (pile_res * 0.45).clamp(0.0, 2.5);
    pile_field.compact_point(dozer_xy, 0.04);
    if blade_down && dozer_speed > 6.0 {
        let front_tip = blade_points.get(2).copied().unwrap_or(dozer_xy);
        pile_field.push_pile_with_blade(front_tip, forward_dir, 20.0, 0.45 * push_force_mul);
    }

    let mut max_penetration_meters = 0.0_f32;
    let mut deepest_contact_normal = Vec2::Y;
    scratch.blast_queue.clear();

    // 1. Evaluate spatial per-cell building structural impacts via 5-point blade contact array & hull (`Issue #14`)
    fill_sim_order(&mut scratch.building_order, building_query.iter().map(|q| q.0), &ordinals);
    fill_sim_order(&mut scratch.prop_order, prop_query.iter().map(|q| q.0), &ordinals);
    fill_sim_order(&mut scratch.rubble_order, sleeping_rubble_query.iter().map(|q| q.0), &ordinals);

    for order_idx in 0..scratch.building_order.len() {
        let Some(&(_, b_key)) = scratch.building_order.get(order_idx) else {
            continue;
        };
        let Ok((b_entity, b_pose, b_tf, b_collider, b_meta, mut cutaway, base_box, assembly)) =
            building_query.get_mut(b_key)
        else {
            continue;
        };
        let b_xy = b_pose.map_or_else(
            || sanitize_vec2(b_tf.translation.truncate(), Vec2::ZERO),
            |p| sanitize_vec2(p.pos, Vec2::ZERO),
        );
        let b_heading = b_pose.map_or_else(
            || sanitize_f32(b_tf.rotation.to_euler(EulerRot::ZYX).0, 0.0),
            |p| sanitize_f32(p.heading_rad, 0.0),
        );
        let b_rot = Rotation::radians(b_heading);
        let b_hit_radius = 85.0 + slam_bonus;
        let blade_hit = blade_points
            .iter()
            .any(|pt| pt.distance_squared(b_xy) <= b_hit_radius * b_hit_radius);
        if !blade_hit && dozer_xy.distance_squared(b_xy) > 95.0 * 95.0 {
            continue;
        }

        let collider_contact = if let (Some(d_col), Some(b_col)) = (dozer_collider, b_collider) {
            avian2d::collision::contact_query::contact(
                d_col,
                dozer_xy,
                dozer_rot,
                b_col,
                b_xy,
                b_rot,
                0.0,
            )
            .ok()
            .flatten()
            .filter(|c| c.penetration > 0.0)
        } else {
            None
        };

        if let Some(contact) = collider_contact {
            let overlap_meters =
                sanitize_f32(contact.penetration / pd_dozer::WORLD_UNITS_PER_METER, 0.0).max(0.0);
            if overlap_meters > max_penetration_meters {
                let raw_normal = -contact.global_normal1(&dozer_rot);
                let safe_normal = sanitize_vec2(raw_normal, Vec2::Y).normalize_or_zero();
                max_penetration_meters = overlap_meters;
                deepest_contact_normal = if safe_normal.length_squared() > 1e-4 {
                    safe_normal
                } else {
                    let fallback_dir =
                        sanitize_vec2((dozer_xy - b_xy).normalize_or_zero(), Vec2::Y);
                    if fallback_dir.length_squared() > 1e-4 {
                        fallback_dir
                    } else {
                        Vec2::Y
                    }
                };
            }
        }

        scratch.destroyed_ids.clear();
        scratch.candidate_cells.clear();
        let mut any_cell_damaged = false;

        if let Some(entry) = graph_registry.buildings.get_mut(&b_entity) {
            let blade_cell_radius = 15.5 + slam_bonus * 0.5;
            let blade_cell_radius_sq = blade_cell_radius * blade_cell_radius;
            let hull_radius_sq = 25.0 * 25.0;

            for (&id, node) in &entry.nodes {
                if !node.intact || node.story_level != 0 {
                    continue;
                }
                let (w_min, w_max) = node.world_bounds(b_xy);
                let min_blade_sq = blade_points
                    .iter()
                    .map(|pt| pt.distance_squared(pt.clamp(w_min, w_max)))
                    .fold(f32::MAX, f32::min);
                let hull_sq = dozer_xy.distance_squared(dozer_xy.clamp(w_min, w_max));

                if min_blade_sq <= blade_cell_radius_sq
                    || (dozer_veh.crush_intersecting_block && hull_sq <= hull_radius_sq)
                {
                    scratch
                        .candidate_cells
                        .push((id, min_blade_sq.min(hull_sq)));
                }
            }

            if !scratch.candidate_cells.is_empty() {
                scratch.candidate_cells.sort_by(|a, b| {
                    a.1.partial_cmp(&b.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.0.cmp(&b.0))
                });

                let base_nodes = dozer_veh.consume_blade_shear_nodes(*blade_state, &upgrades);
                let max_nodes_per_hit =
                    if dozer_veh.crush_intersecting_block && collider_contact.is_some() {
                        base_nodes.saturating_add(4)
                    } else {
                        base_nodes
                    };

                let blade_mul = if blade_down { 1.45 } else { 0.90 };
                let cell_damage = (32.0 + dozer_speed * 0.35 + slam_bonus * 3.5)
                    * safe_upgrades.ar400_edge
                    * push_force_mul
                    * blade_mul;

                let scratch_ref = &mut *scratch;
                for &(cell_id, _) in scratch_ref.candidate_cells.iter().take(max_nodes_per_hit) {
                    if let Some(node) = entry.nodes.get_mut(&cell_id) {
                        any_cell_damaged = true;
                        if node.apply_localized_damage(cell_damage) {
                            scratch_ref.destroyed_ids.push(cell_id);
                        }
                    }
                }
            }

            if any_cell_damaged {
                entry.visual_revision = entry.visual_revision.saturating_add(1);
                entry.refresh_surface_cache();
            }
        }

        if any_cell_damaged {
            contact_load += 1.5;
            let recoil_dir = (dozer_xy - b_xy).normalize_or_zero();
            let recoil_bounce = (0.12 / push_force_mul).clamp(0.02, 0.12);
            let veh = &mut *dozer_veh;
            let mut pos_mut = dozer_xy;
            let mut vel_mut = Vec2::new(veh.vx, veh.vy);
            let _ = apply_dozer_impulse(
                &mut pos_mut,
                &mut vel_mut,
                dozer_heading,
                &mut veh.pitch,
                &mut veh.roll,
                &mut veh.last_impact,
                recoil_dir,
                0.0,
                recoil_bounce,
            );
            veh.vx = vel_mut.x;
            veh.vy = vel_mut.y;
            cutaway.exterior_breached = true;
            audio_events.send(AudioTriggerEvent {
                kind: AudioSfxKind::StructuralCrunch,
            });
            particle_events.send(ParticleBurstEvent {
                kind: ParticleBurstKind::MasonryDust,
                world_pos: b_xy,
                count: 5,
            });
        }

        if !scratch.destroyed_ids.is_empty() {
            for &id in &scratch.destroyed_ids {
                let (cash_val, cell_world, cell_mat) = graph_registry
                    .buildings
                    .get(&b_entity)
                    .and_then(|e| e.nodes.get(&id))
                    .map_or((16, b_xy, pd_core::MaterialKind::Concrete), |n| {
                        (n.cash_value, n.world_center(b_xy), n.material)
                    });
                let cell_screen = world_to_iso_screen(cell_world, 8.0);
                cash_events.send(DemolitionCashEvent {
                    amount: cash_val,
                    world_pos: cell_screen.extend(0.0),
                });
                spawn_pooled_debris_chunk_with_material(
                    &mut commands,
                    &mut debris_pool,
                    *epoch,
                    cell_world,
                    0,
                    cell_mat,
                    cell_mat.density(),
                    false,
                    Some(forward_dir * 22.0),
                );
            }

            let _ = queue_structural_integrity_evaluation(
                b_entity,
                &scratch.destroyed_ids,
                &mut graph_registry,
                task_pool,
            );
        }

        if any_cell_damaged && let Some(entry) = graph_registry.buildings.get(&b_entity) {
            if !scratch.destroyed_ids.is_empty()
                && let Some(mut b_cmds) = commands.get_entity(b_entity)
            {
                if let Some(new_col) = build_building_compound_collider(&entry.nodes) {
                    b_cmds.insert(new_col);
                } else {
                    b_cmds
                        .remove::<RigidBody>()
                        .remove::<Collider>()
                        .remove::<Position>()
                        .remove::<Rotation>();
                }
            }
            if let (Some(mut b_box), Some(mut asm)) = (base_box, assembly) {
                let (mut new_base, mut new_asm) = build_building_iso_assembly_from_entry(
                    entry,
                    b_meta.stories,
                    b_meta.palette_index,
                );
                new_base.alpha = cutaway.current_roof_alpha;
                new_asm.alpha_multiplier = cutaway.current_roof_alpha;
                *b_box = new_base;
                *asm = new_asm;
            }
        }
    }

    // 2. Evaluate interior furniture & exterior street prop impacts using the 7 destruction profiles (`Issue #17`)
    let prop_damage = (38.0 + dozer_speed * 0.4 + slam_bonus * 4.0)
        * safe_upgrades.ar400_edge
        * push_force_mul;
    for order_idx in 0..scratch.prop_order.len() {
        let Some(&(_, p_key)) = scratch.prop_order.get(order_idx) else {
            continue;
        };
        let Ok((prop_entity, mut prop_pose, prop_tf, mut prop, mut prop_box)) =
            prop_query.get_mut(p_key)
        else {
            continue;
        };
        let p_xy = prop_pose.as_deref().map_or_else(
            || sanitize_vec2(prop_tf.translation.truncate(), Vec2::ZERO),
            |p| sanitize_vec2(p.pos, Vec2::ZERO),
        );
        let p_hit_radius = 32.0 + slam_bonus;
        let blade_hit = blade_points
            .iter()
            .any(|pt| pt.distance_squared(p_xy) <= p_hit_radius * p_hit_radius);
        if blade_hit || dozer_xy.distance_squared(p_xy) <= 42.0 * 42.0 {
            contact_load += 0.35;
            let prop_recoil_bounce = (0.08 / push_force_mul).clamp(0.015, 0.08);
            let veh = &mut *dozer_veh;
            let mut pos_mut = dozer_xy;
            let mut vel_mut = Vec2::new(veh.vx, veh.vy);
            let _ = apply_dozer_impulse(
                &mut pos_mut,
                &mut vel_mut,
                dozer_heading,
                &mut veh.pitch,
                &mut veh.roll,
                &mut veh.last_impact,
                (dozer_xy - p_xy).normalize_or_zero(),
                0.0,
                prop_recoil_bounce,
            );
            veh.vx = vel_mut.x;
            veh.vy = vel_mut.y;

            let impact_dir = (p_xy - dozer_xy).normalize_or_zero();
            let (_destroyed, stress_delta, maybe_blast) = apply_prop_damage_and_profile(
                &mut commands,
                *epoch,
                prop_entity,
                p_xy,
                &mut prop,
                prop_pose.as_deref_mut(),
                prop_box.as_deref_mut(),
                prop_damage,
                impact_dir,
                blade_down,
                &mut debris_pool,
                &mut pile_field,
                &mut cash_events,
                &mut decal_events,
                &mut particle_events,
                &mut audio_events,
            );
            if stress_delta > 0.0 {
                stress.track_stress_pct =
                    sanitize_f32(stress.track_stress_pct + stress_delta, stress.track_stress_pct)
                        .clamp(0.0, 1.0);
            }
            if let Some(blast) = maybe_blast {
                scratch.blast_queue.push(blast);
            }
        }
    }

    // 2b. Process chained Explosive prop shockwaves (`Issue #17`, bounded by `MAX_EXPLOSION_GENERATION`)
    let mut blast_idx = 0_usize;
    while blast_idx < scratch.blast_queue.len() && blast_idx < 16 {
        let Some(&(blast_center, blast_radius, blast_dmg, generation)) =
            scratch.blast_queue.get(blast_idx)
        else {
            break;
        };
        blast_idx = blast_idx.saturating_add(1);
        let r_sq = blast_radius * blast_radius;

        // Damage nearby building ground/lower cells within blast_radius
        for order_idx in 0..scratch.building_order.len() {
            let Some(&(_, b_key)) = scratch.building_order.get(order_idx) else {
                continue;
            };
            let Ok((b_entity, b_pose, b_tf, _, b_meta, cutaway, base_box, assembly)) =
                building_query.get_mut(b_key)
            else {
                continue;
            };
            let b_xy = b_pose.map_or_else(
                || sanitize_vec2(b_tf.translation.truncate(), Vec2::ZERO),
                |p| sanitize_vec2(p.pos, Vec2::ZERO),
            );
            scratch.destroyed_ids.clear();
            let mut b_damaged = false;
            if let Some(entry) = graph_registry.buildings.get_mut(&b_entity) {
                for (&nid, node) in &mut entry.nodes {
                    if node.intact && node.story_level <= 1 {
                        let c_world = node.world_center(b_xy);
                        let d_sq = c_world.distance_squared(blast_center);
                        if d_sq <= r_sq {
                            b_damaged = true;
                            let falloff = (1.0 - (d_sq.sqrt() / blast_radius.max(1.0)) * 0.5)
                                .clamp(0.35, 1.0);
                            if node.apply_localized_damage(blast_dmg * falloff) {
                                scratch.destroyed_ids.push(nid);
                            }
                        }
                    }
                }
                if b_damaged {
                    entry.visual_revision = entry.visual_revision.saturating_add(1);
                    entry.refresh_surface_cache();
                }
            }
            if !scratch.destroyed_ids.is_empty() {
                let _ = queue_structural_integrity_evaluation(
                    b_entity,
                    &scratch.destroyed_ids,
                    &mut graph_registry,
                    task_pool,
                );
            }
            if b_damaged && let Some(entry) = graph_registry.buildings.get(&b_entity) {
                if !scratch.destroyed_ids.is_empty()
                    && let Some(mut b_cmds) = commands.get_entity(b_entity)
                {
                    if let Some(new_col) = build_building_compound_collider(&entry.nodes) {
                        b_cmds.insert(new_col);
                    } else {
                        b_cmds
                            .remove::<RigidBody>()
                            .remove::<Collider>()
                            .remove::<Position>()
                            .remove::<Rotation>();
                    }
                }
                if let (Some(mut b_box), Some(mut asm)) = (base_box, assembly) {
                    let (mut new_base, mut new_asm) = build_building_iso_assembly_from_entry(
                        entry,
                        b_meta.stories,
                        b_meta.palette_index,
                    );
                    new_base.alpha = cutaway.current_roof_alpha;
                    new_asm.alpha_multiplier = cutaway.current_roof_alpha;
                    *b_box = new_base;
                    *asm = new_asm;
                }
            }
        }

        // Chain blast to nearby props if generation < MAX_EXPLOSION_GENERATION
        if generation < MAX_EXPLOSION_GENERATION {
            for order_idx in 0..scratch.prop_order.len() {
                let Some(&(_, p_key)) = scratch.prop_order.get(order_idx) else {
                    continue;
                };
                let Ok((prop_entity, mut prop_pose, prop_tf, mut prop, mut prop_box)) =
                    prop_query.get_mut(p_key)
                else {
                    continue;
                };
                if prop.hp <= 0.0 {
                    continue;
                }
                let p_xy = prop_pose.as_deref().map_or_else(
                    || sanitize_vec2(prop_tf.translation.truncate(), Vec2::ZERO),
                    |p| sanitize_vec2(p.pos, Vec2::ZERO),
                );
                let d_sq = p_xy.distance_squared(blast_center);
                if d_sq > 1.0 && d_sq <= r_sq {
                    prop.chain_generation = generation.saturating_add(1);
                    let dir = (p_xy - blast_center).normalize_or_zero();
                    let (_, _, next_blast) = apply_prop_damage_and_profile(
                        &mut commands,
                        *epoch,
                        prop_entity,
                        p_xy,
                        &mut prop,
                        prop_pose.as_deref_mut(),
                        prop_box.as_deref_mut(),
                        blast_dmg,
                        dir,
                        true,
                        &mut debris_pool,
                        &mut pile_field,
                        &mut cash_events,
                        &mut decal_events,
                        &mut particle_events,
                        &mut audio_events,
                    );
                    if let Some(nb) = next_blast {
                        scratch.blast_queue.push(nb);
                    }
                }
            }
        }
    }

    // 3. Tier 2 -> Tier 3 Heightmap & PileField Crushing: Driving over sleeping rubble with BladeDown
    // transfers its conserved mass into `PileField` and spawns a ground decal scar (`Issue #16`).
    let r_hit_radius = 30.0 + slam_bonus;
    for order_idx in 0..scratch.rubble_order.len() {
        let Some(&(_, r_key)) = scratch.rubble_order.get(order_idx) else {
            continue;
        };
        let Ok((rubble_entity, rubble_pose, rubble_tf, mut chunk)) =
            sleeping_rubble_query.get_mut(r_key)
        else {
            continue;
        };
        let r_xy = rubble_pose.map_or_else(
            || sanitize_vec2(rubble_tf.translation.truncate(), Vec2::ZERO),
            |p| sanitize_vec2(p.pos, Vec2::ZERO),
        );
        if dozer_xy.distance_squared(r_xy) <= 52.0 * 52.0 {
            chunk.recently_touched_secs = 1.2;
            chunk.unattended_secs = 0.0;
        }
        if !blade_down || chunk.tier != DebrisTier::SleepingInstanced {
            continue;
        }
        let blade_hit = blade_points
            .iter()
            .any(|pt| pt.distance_squared(r_xy) <= r_hit_radius * r_hit_radius);
        if blade_hit || dozer_xy.distance_squared(r_xy) <= 38.0 * 38.0 {
            chunk.tier = DebrisTier::CrushedDecal;
            pile_field.add_mass(r_xy, chunk.mass, chunk.material);
            debris_pool.active_ring.retain(|&e| e != rubble_entity);
            decal_events.send(DecalSpawnRequest {
                kind: DecalKind::CrushedRubbleScar,
                world_pos: r_xy,
                size: Vec2::new(18.0, 12.0),
                rotation_rad: 0.0,
            });
            if let Some(e_cmds) = commands.get_entity(rubble_entity) {
                e_cmds.despawn_recursive();
            }
        }
    }

    dozer_veh.crush_intersecting_block = false;
    track_input.external_load_factor = contact_load.clamp(0.0, 5.0);
    if max_penetration_meters > track_input.penetration_depth {
        track_input.penetration_depth = max_penetration_meters;
        track_input.contact_normal = deepest_contact_normal;
    }
}

/// Freezes the Avian2D clock on every fixed tick where the simulation does not run.
///
/// Gameplay systems run under `pd_core::sim_running` (`Playing`, no `SimHold`, no pending
/// restart), but Avian steps in `FixedPostUpdate` regardless. Without this gate, bodies keep
/// integrating their last velocity through Pause, the upgrade modal and the Title screen, for
/// as many ticks as the wall-clock pause lasted. Runs in `FixedFirst`, so a hold raised during
/// tick `k` still lets tick `k` finish its physics step and stops every later tick.
///
/// Pausing also zeroes the clock's delta: Avian 0.2's `run_physics_schedule` decides whether to
/// step from `Time<Physics>::delta()` *before* it resets a paused clock's delta, so a bare
/// `pause()` would still run one more full step on the first paused tick.
pub fn gate_physics_clock_on_sim_running_system(
    state: Option<Res<State<AppState>>>,
    hold: Option<Res<pd_core::SimHold>>,
    restart_gate: Option<Res<pd_core::RestartGate>>,
    freeze: Option<Res<pd_core::SimFreeze>>,
    mut physics_time: ResMut<Time<Physics>>,
) {
    let running = pd_core::sim_running(state, hold, restart_gate, freeze);
    if running && physics_time.is_paused() {
        physics_time.unpause();
    } else if !running && !physics_time.is_paused() {
        physics_time.pause();
        physics_time.advance_by(std::time::Duration::ZERO);
    }
}

/// Registers [`gate_physics_clock_on_sim_running_system`] at the start of every fixed tick.
pub struct PhysicsStateGatePlugin;

impl Plugin for PhysicsStateGatePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(FixedFirst, gate_physics_clock_on_sim_running_system);
    }
}

/// Adds the full deterministic simulation (Avian2D, the six `pd_*` crates, the physics state
/// gate and the structural impact pass) to `app`. The caller supplies the windowing/render or
/// headless base plugins. Shared by the game binary, headless tests and the scenario runner.
pub fn add_simulation_plugins(app: &mut App) {
    app.add_plugins(PhysicsPlugins::default().with_length_unit(16.0))
        .insert_resource(Gravity(Vec2::ZERO))
        .add_plugins(PhysicsStateGatePlugin)
        .add_plugins((
            PdCorePlugin,
            PdWorldPlugin,
            PdDozerPlugin,
            PdDestructionPlugin,
            PdAiPlugin,
            PdUiPlugin,
        ))
        .add_systems(
            FixedUpdate,
            evaluate_dozer_structural_impacts_system
                .run_if(in_state(AppState::Playing))
                .in_set(PdSystemSet::StructuralImpacts),
        );
}

/// Adds the presentation layer used by the windowed game: isometric camera follow/cutaways
/// and synthesized audio playback.
pub fn add_presentation_plugins(app: &mut App) {
    app.init_resource::<SynthesizedAudioBank>()
        .add_systems(
            Startup,
            (
                setup_isometric_camera_system,
                init_synthesized_audio_bank_system,
            ),
        )
        .add_systems(
            Update,
            (
                camera_follow_and_cutaway_system.run_if(in_state(AppState::Playing)),
                play_synthesized_audio_events_system,
            )
                .chain()
                .after(PdSystemSet::HudInterpolation),
        );
}

/// Adds the in-game record/replay tooling: `pd_replay`, F9 record / F10 replay with the
/// REC / MATCH / DRIFT badge, and the F4 isometric collider overlay.
pub fn add_debug_tooling_plugins(app: &mut App) {
    app.add_plugins((
        pd_replay::PdReplayPlugin,
        overlays::DebugOverlaysPlugin,
        replay_hud::ReplayHudPlugin,
    ));
}

/// Builds a windowless app running the full simulation in lockstep (exactly one 60 Hz tick per
/// `update()`), for deterministic tests and replays. Rendering, audio and windows are absent.
#[must_use]
pub fn headless_sim_app() -> App {
    AsyncComputeTaskPool::get_or_init(|| bevy::tasks::TaskPoolBuilder::new().num_threads(2).build());
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin::default(),
        bevy::scene::ScenePlugin,
        bevy::state::app::StatesPlugin,
        TransformPlugin,
        HierarchyPlugin,
        bevy::diagnostic::DiagnosticsPlugin,
    ))
    .init_asset::<Image>()
    .init_asset::<TextureAtlasLayout>()
    .init_asset::<Mesh>()
    .init_asset::<ColorMaterial>()
    .init_resource::<ButtonInput<KeyCode>>()
    .insert_resource(lockstep_time_strategy());
    add_simulation_plugins(&mut app);
    app
}

/// `TimeUpdateStrategy` that advances exactly one simulation tick per frame.
#[must_use]
pub fn lockstep_time_strategy() -> bevy::time::TimeUpdateStrategy {
    bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_secs_f64(
        1.0 / pd_core::SIM_HZ,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::audio::Decodable;
    use bevy::state::app::StatesPlugin;
    use bevy::tasks::TaskPoolBuilder;
    use pd_core::{
        CampaignLevel, CashLedger, CashPopupPool, EnvironmentConfig, MAX_ODOMETER_CASH,
        PhysicsError, SessionEpoch, SiteRestartRequested, SiteScoped, try_award_demolition_cash,
    };
    use pd_dozer::{
        BladeState, MechanicalStress, TrackInput, UpgradeMultipliers, compute_skid_steer_kinematics,
    };
    use pd_world::GroundDecalPool;

    /// Gate 2 Verification Contract: `test_kinematic_and_ledger_extremes`
    /// Feeds extreme/corrupted inputs (`dt = 0.0`, `dt = 999.0`, `NaN` vectors, `u32::MAX` cash
    /// additions, disconnected single-node buildings) into core functions and asserts clean
    /// `Err(...)` or clamped `Ok(...)` returns without panicking.
    #[test]
    fn test_kinematic_and_ledger_extremes() {
        let mut stress = MechanicalStress::default();
        let upgrades = UpgradeMultipliers {
            ar400_edge: f32::NAN,
            charge_air_kit: f32::INFINITY,
            aux_ram: -500.0,
        };
        let valid_input = TrackInput {
            left_track: 1.0,
            right_track: -1.0,
            heading_radians: 0.5,
            external_load_factor: 999.0,
            penetration_depth: 2.0,
            contact_normal: Vec2::new(f32::NAN, f32::INFINITY),
            ..Default::default()
        };

        // 1. dt == 0.0 -> clean Err
        assert_eq!(
            compute_skid_steer_kinematics(
                &valid_input,
                BladeState::BladeDown,
                &upgrades,
                &mut stress,
                0.0
            ),
            Err(PhysicsError::InvalidDeltaTime(0.0))
        );

        // 2. dt == 999.0 -> clamped Ok with finite velocity
        let extreme_dt_res = compute_skid_steer_kinematics(
            &valid_input,
            BladeState::BladeDown,
            &upgrades,
            &mut stress,
            999.0,
        );
        assert!(extreme_dt_res.is_ok());
        if let Ok(out) = extreme_dt_res {
            assert!(out.linear_velocity.is_finite());
            assert!(out.angular_velocity.is_finite());
        }

        // 3. NaN track input -> clean Err(PhysicsError::NonFiniteInput)
        let nan_input = TrackInput {
            left_track: f32::NAN,
            ..valid_input
        };
        assert_eq!(
            compute_skid_steer_kinematics(
                &nan_input,
                BladeState::BladeUp,
                &upgrades,
                &mut stress,
                0.016
            ),
            Err(PhysicsError::NonFiniteInput)
        );

        // 4. u32::MAX cash addition with NaN world_pos -> clamped to $999,999 without panicking
        let mut ledger = CashLedger::default();
        let mut pool = CashPopupPool::default();
        let cash_res = try_award_demolition_cash(
            &mut ledger,
            &mut pool,
            u32::MAX,
            Vec3::new(f32::NAN, f32::INFINITY, 0.0),
        );
        assert_eq!(cash_res, Ok(MAX_ODOMETER_CASH));

        // 5. WAV synthesizer handles NaN/extreme inputs without panicking and decodes via rodio
        let wav = synthesize_wav_sfx_bytes(f32::NAN, f32::INFINITY, -10.0);
        assert!(wav.len() > 44);
        let source = AudioSource { bytes: wav.into() };
        assert!(source.decoder().next().is_some());
    }

    /// Gate 3 Verification Contract: `test_idempotent_site_restart`
    /// Headless Bevy world test that spawns a full city block, triggers a building collapse,
    /// dispatches `Restart Site` mid-collapse, ticks 10 frames, and asserts that the entity count
    /// and state match a clean initial spawn with zero orphaned debris or stale async tasks.
    #[test]
    fn test_idempotent_site_restart() {
        let task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPoolBuilder::new().build());

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin))
            .add_plugins((
                PdCorePlugin,
                PdWorldPlugin,
                PdDozerPlugin,
                PdDestructionPlugin,
                PdAiPlugin,
                PdUiPlugin,
            ));

        // Transition directly into Playing state and tick 3 frames to spawn the initial city block.
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        for _ in 0..3 {
            app.update();
        }

        let initial_epoch = *app.world().resource::<SessionEpoch>();
        let initial_site_entity_count = app
            .world_mut()
            .query_filtered::<Entity, With<SiteScoped>>()
            .iter(app.world())
            .count();
        assert!(initial_site_entity_count > 0);

        // Trigger a building collapse + spawn active debris mid-session.
        let first_building = app
            .world_mut()
            .query_filtered::<Entity, With<ModularBuilding>>()
            .iter(app.world())
            .next();
        assert!(first_building.is_some());

        if let Some(b_entity) = first_building {
            let mut registry = app.world_mut().resource_mut::<BuildingGraphRegistry>();
            let status = queue_structural_integrity_evaluation(
                b_entity,
                &[0, 1, 2, 3, 4],
                &mut registry,
                task_pool,
            );
            assert_eq!(status, pd_destruction::EvalStatus::Dispatched);
        }

        // Dispatch `Restart Site` while collapse evaluation is mid-flight!
        app.world_mut()
            .send_event(SiteRestartRequested {
                preserve_layout: true,
            });

        // Tick 10 frames as required by Gate 3.
        for _ in 0..10 {
            app.update();
        }

        let post_restart_epoch = *app.world().resource::<SessionEpoch>();
        assert!(post_restart_epoch > initial_epoch);

        // Verify zero entities from the old SessionEpoch remain in the world!
        for scoped in app.world_mut().query::<&SiteScoped>().iter(app.world()) {
            assert_eq!(scoped.epoch, post_restart_epoch);
        }

        // Verify cash ledger and debris pool were cleanly reset.
        let ledger = app.world().resource::<CashLedger>();
        assert_eq!(ledger.current_cash, 0);
        let debris_pool = app.world().resource::<DebrisPoolRegistry>();
        assert_eq!(debris_pool.active_ring.len(), 0);
    }

    /// End-to-End Vertical Slice Verification: Simulates all 7 campaign levels (`1/7 County`
    /// through `7/7 Governor's Mansion`), dozer blade impacts, prop destruction, and 3-tier
    /// sleeping rubble crushing into ground decals.
    #[test]
    fn test_full_vertical_slice_all_levels_and_rubble_crushing() {
        let _task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPoolBuilder::new().build());

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin))
            .add_plugins((
                PdCorePlugin,
                PdWorldPlugin,
                PdDozerPlugin,
                PdDestructionPlugin,
                PdAiPlugin,
                PdUiPlugin,
            ))
            .add_systems(
                Update,
                evaluate_dozer_structural_impacts_system
                    .after(PdSystemSet::WorldTilemapSync),
            );

        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);

        // Cycle through all 7 campaign levels and verify clean spawning
        for level in CampaignLevel::ALL {
            app.world_mut().resource_mut::<EnvironmentConfig>().level = level;
            app.world_mut().send_event(SiteRestartRequested {
                preserve_layout: false,
            });
            for _ in 0..3 {
                app.update();
            }
            let building_count = app
                .world_mut()
                .query_filtered::<Entity, With<ModularBuilding>>()
                .iter(app.world())
                .count();
            assert_eq!(building_count, 3);
        }

        // Lower blade to BladeDown, spawn sleeping rubble at dozer position, and verify Tier 3 crushing!
        *app.world_mut().resource_mut::<BladeState>() = BladeState::BladeDown;
        let epoch = *app.world().resource::<SessionEpoch>();
        app.world_mut().spawn((
            SiteScoped { epoch },
            DebrisTier::SleepingInstanced,
            DebrisChunk {
                tier: DebrisTier::SleepingInstanced,
                active_timer_secs: 0.0,
                simulated_z_height: 0.0,
                vertical_z_velocity: 0.0,
                horizontal_velocity: Vec2::ZERO,
                spin_rate: 0.0,
                ..Default::default()
            },
            WorldPose::from_xy(0.0, -60.0),
            Transform::from_xyz(0.0, -60.0, 0.0),
            GlobalTransform::default(),
        ));

        for _ in 0..3 {
            app.update();
        }

        let decal_pool = app.world().resource::<GroundDecalPool>();
        assert!(decal_pool.total_crushed_scars >= 1);
    }

    /// Regression Test: Exercises the `AppState::Title` -> `AppState::Playing` transition with
    /// `PhysicsPlugins` (`avian2d`), `AssetPlugin`, and `AudioPlugin` active while dispatching
    /// `SiteRestartRequested` and `AudioTriggerEvent`.
    #[test]
    fn test_title_to_playing_transition_with_physics() {
        let _task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPoolBuilder::new().build());

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            bevy::audio::AudioPlugin::default(),
            bevy::scene::ScenePlugin,
            StatesPlugin,
            TransformPlugin,
            HierarchyPlugin,
            bevy::diagnostic::DiagnosticsPlugin,
        ))
        .init_asset::<Image>()
        .init_asset::<TextureAtlasLayout>()
        .init_asset::<Mesh>()
        .init_asset::<ColorMaterial>()
        .add_plugins(PhysicsPlugins::default().with_length_unit(16.0))
        .insert_resource(Gravity(Vec2::ZERO))
        .init_resource::<SynthesizedAudioBank>()
        .init_resource::<ButtonInput<KeyCode>>()
        .add_plugins((
            PdCorePlugin,
            PdWorldPlugin,
            PdDozerPlugin,
            PdDestructionPlugin,
            PdAiPlugin,
            PdUiPlugin,
        ))
        .add_systems(
            Startup,
            (
                setup_isometric_camera_system,
                init_synthesized_audio_bank_system,
            ),
        )
        .add_systems(
            Update,
            (
                camera_follow_and_cutaway_system.run_if(in_state(AppState::Playing)),
                play_synthesized_audio_events_system,
            )
                .chain()
                .after(PdSystemSet::HudInterpolation),
        )
        .add_systems(
            FixedUpdate,
            evaluate_dozer_structural_impacts_system
                .run_if(in_state(AppState::Playing))
                .before(PdSystemSet::DestructionGraph)
                .after(PdSystemSet::DozerKinematics),
        );

        // Tick 3 frames in AppState::Title
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(18));
            app.update();
        }
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Title);

        // Simulate clicking the PLAY button / pressing Enter on the Title Screen:
        for (mut interaction, action) in app
            .world_mut()
            .query::<(&mut Interaction, &pd_ui::HudButtonAction)>()
            .iter_mut(app.world_mut())
        {
            if *action == pd_ui::HudButtonAction::PlayFromTitle {
                *interaction = Interaction::Pressed;
            }
        }
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Enter);

        for _ in 0..8 {
            std::thread::sleep(std::time::Duration::from_millis(18));
            app.update();
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .clear();
        }

        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Playing);
        let dozer_count = app
            .world_mut()
            .query_filtered::<Entity, With<PlayerVehicle>>()
            .iter(app.world())
            .count();
        assert_eq!(dozer_count, 1);
        for (pose, pos, tf) in app
            .world_mut()
            .query_filtered::<(&WorldPose, &Position, &Transform), With<PlayerVehicle>>()
            .iter(app.world())
        {
            assert!(pos.0.distance(pose.pos) < 1e-3);
            let expected_screen = world_to_iso_screen(pose.pos, pose.elev_z);
            assert!(tf.translation.truncate().distance(expected_screen) < 1e-3);
        }

        let building_count = app
            .world_mut()
            .query_filtered::<Entity, With<ModularBuilding>>()
            .iter(app.world())
            .count();
        assert_eq!(building_count, 3);
        for (pose, pos, tf) in app
            .world_mut()
            .query_filtered::<(&WorldPose, &Position, &Transform), With<ModularBuilding>>()
            .iter(app.world())
        {
            assert!(pos.0.distance(pose.pos) < 1e-3);
            let expected_screen = world_to_iso_screen(pose.pos, pose.elev_z);
            assert!(tf.translation.truncate().distance(expected_screen) < 1e-3);
            // Screen-space Y is non-equal to Cartesian Y for non-origin buildings!
            assert!((pos.0.y - tf.translation.y).abs() > 1.0);
        }
    }

    /// Epic #1 Verification Test: Verifies that `WorldPose` decoupling (#7), 2:1 isometric
    /// diamond ground/decal meshes (#8), `IsoExtrudedBox` 2.5D meshes with upper-west sun
    /// lighting (#9), and multi-part `IsoBoxAssembly` visuals on the dozer, buildings, props,
    /// vehicles, pedestrians, and debris (#10) are spawned and synchronized accurately.
    #[test]
    fn test_epic_1_isometric_projection_and_extruded_meshes() {
        let _task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPoolBuilder::new().build());

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            StatesPlugin,
            TransformPlugin,
            HierarchyPlugin,
        ))
        .init_asset::<Image>()
        .init_asset::<TextureAtlasLayout>()
        .init_asset::<Mesh>()
        .init_asset::<ColorMaterial>()
        .add_plugins((
            PdCorePlugin,
            PdWorldPlugin,
            PdDozerPlugin,
            PdDestructionPlugin,
            PdAiPlugin,
            PdUiPlugin,
        ));

        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        for _ in 0..4 {
            app.update();
        }

        // Verify dozer has WorldPose, IsoExtrudedBox, IsoBoxAssembly, Mesh2d, and IsoMeshCacheKey
        let mut dozer_q = app.world_mut().query_filtered::<(
            &WorldPose,
            &Transform,
            &IsoExtrudedBox,
            &IsoBoxAssembly,
            &Mesh2d,
            &pd_world::IsoMeshCacheKey,
        ), With<PlayerVehicle>>();
        let dozer_item = dozer_q.iter(app.world()).next();
        assert!(dozer_item.is_some());
        if let Some((pose, tf, _, asm, _, cache_key)) = dozer_item {
            let expected_screen = world_to_iso_screen(pose.pos, pose.elev_z);
            assert!((tf.translation.x - expected_screen.x).abs() < 1e-3);
            assert!((tf.translation.y - expected_screen.y).abs() < 1e-3);
            assert!(asm.parts.len() >= 20);
            let blade_up_hash = cache_key.assembly_hash;

            // Verify toggling BladeDown produces a distinct assembly hash
            let (down_base, down_asm) = pd_dozer::build_dozer_iso_assembly(BladeState::BladeDown);
            let down_key = pd_world::IsoMeshCacheKey::compute(pose, &down_base, Some(&down_asm));
            assert_ne!(blade_up_hash, down_key.assembly_hash);
        }

        // Verify buildings, props, and AI agents all have 2.5D IsoExtrudedBox + Mesh2d
        let extruded_mesh_count = app
            .world_mut()
            .query_filtered::<Entity, (With<IsoExtrudedBox>, With<Mesh2d>)>()
            .iter(app.world())
            .count();
        assert!(extruded_mesh_count >= 25);
    }

    /// Epic #2 Verification Test: Verifies timed powered blade surge & 5-point blade contact (#11),
    /// heavy machinery momentum, turn scrub & spring-damper chassis pitch/roll (#12), and
    /// milestone upgrade choices (`1`/`2`/`3`) with Campaign modal pause & level retry reset (#13).
    #[test]
    fn test_epic_2_blade_points_timed_push_momentum_and_milestone_upgrades() {
        let _task_pool = AsyncComputeTaskPool::get_or_init(|| TaskPoolBuilder::new().build());

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            StatesPlugin,
            TransformPlugin,
            HierarchyPlugin,
        ))
        .init_asset::<Image>()
        .init_asset::<TextureAtlasLayout>()
        .init_asset::<Mesh>()
        .init_asset::<ColorMaterial>()
        .init_resource::<ButtonInput<KeyCode>>()
        // Lockstep: one 60 Hz simulation tick per update (cash and milestones are tick-based).
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_secs_f64(1.0 / pd_core::SIM_HZ),
        ))
        .add_plugins((
            PdCorePlugin,
            PdWorldPlugin,
            PdDozerPlugin,
            PdDestructionPlugin,
            PdAiPlugin,
            PdUiPlugin,
        ))
        .add_systems(
            Update,
            evaluate_dozer_structural_impacts_system.after(PdSystemSet::WorldTilemapSync),
        );

        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        for _ in 0..3 {
            app.update();
        }

        // 1. Verify reaching $350 in Campaign mode does NOT passively auto-scale UpgradeMultipliers,
        //    and instead queues a pending upgrade choice and pauses the simulation.
        app.world_mut().send_event(DemolitionCashEvent {
            amount: 400,
            world_pos: Vec3::ZERO,
        });
        for _ in 0..3 {
            app.update();
        }

        let upgrades = *app.world().resource::<UpgradeMultipliers>();
        assert!((upgrades.ar400_edge - 1.0).abs() < 1e-5);
        assert!((upgrades.charge_air_kit - 1.0).abs() < 1e-5);
        assert!((upgrades.aux_ram - 1.0).abs() < 1e-5);
        let prog = app.world().resource::<pd_dozer::UpgradeProgression>();
        assert_eq!(prog.pending_upgrade_choices, 1);
        assert_eq!(prog.next_milestone_cash, 900);
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Paused);

        // Verify Escape cannot bypass a pending Campaign upgrade choice while paused
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Escape);
        for _ in 0..3 {
            app.update();
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .clear();
        }
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Paused);
        assert_eq!(
            app.world()
                .resource::<pd_dozer::UpgradeProgression>()
                .pending_upgrade_choices,
            1
        );

        // 2. Press '1' (AR400 Edge) to select the upgrade and resume Playing.
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Digit1);
        for _ in 0..3 {
            app.update();
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .clear();
        }

        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Playing);
        let upgrades_after = *app.world().resource::<UpgradeMultipliers>();
        assert!((upgrades_after.ar400_edge - 1.42).abs() < 1e-5);

        // 3. Verify 5-point blade contact crushes rubble located ahead of the dozer center (+26.4 world units)
        *app.world_mut().resource_mut::<BladeState>() = BladeState::BladeDown;
        let epoch = *app.world().resource::<SessionEpoch>();
        let initial_scars = app.world().resource::<GroundDecalPool>().total_crushed_scars;
        // Dozer spawns at (0.0, -60.0) with yaw = 0.0 (+X forward); front blade center is at (26.4, -60.0).
        // Put sleeping rubble at (52.0, -60.0): distance from dozer center is 52.0 (> 38.0 hull radius),
        // but distance from front blade point (26.4, -60.0) is 25.6 (<= 30.0 blade hit radius)!
        app.world_mut().spawn((
            SiteScoped { epoch },
            DebrisTier::SleepingInstanced,
            DebrisChunk {
                tier: DebrisTier::SleepingInstanced,
                active_timer_secs: 0.0,
                simulated_z_height: 0.0,
                vertical_z_velocity: 0.0,
                horizontal_velocity: Vec2::ZERO,
                spin_rate: 0.0,
                ..Default::default()
            },
            WorldPose::from_xy(52.0, -60.0),
            Transform::from_xyz(52.0, -60.0, 0.0),
            GlobalTransform::default(),
        ));
        for _ in 0..3 {
            app.update();
        }
        let scars_after = app.world().resource::<GroundDecalPool>().total_crushed_scars;
        assert!(scars_after > initial_scars);

        // Verify that high Aux Ram (`25.0x`) on a blade-landing tick does NOT inflate the contact radius
        // to crush distant rubble (e.g., 120.0 units ahead of the dozer center).
        app.world_mut().resource_mut::<UpgradeMultipliers>().aux_ram = pd_dozer::MAX_SAFE_MULTIPLIER;
        for mut veh in app
            .world_mut()
            .query::<&mut PlayerVehicle>()
            .iter_mut(app.world_mut())
        {
            veh.pushing_force_multiplier = pd_dozer::MAX_SAFE_MULTIPLIER;
            veh.blade_landed_this_tick = true;
        }
        let far_rubble = app
            .world_mut()
            .spawn((
                SiteScoped { epoch },
                DebrisTier::SleepingInstanced,
                DebrisChunk {
                    tier: DebrisTier::SleepingInstanced,
                    active_timer_secs: 0.0,
                    simulated_z_height: 0.0,
                    vertical_z_velocity: 0.0,
                    horizontal_velocity: Vec2::ZERO,
                    spin_rate: 0.0,
                    ..Default::default()
                },
                WorldPose::from_xy(120.0, -60.0),
                Transform::from_xyz(120.0, -60.0, 0.0),
                GlobalTransform::default(),
            ))
            .id();
        app.update();
        assert!(app.world().get_entity(far_rubble).is_ok());
        app.world_mut().resource_mut::<UpgradeMultipliers>().aux_ram = 1.0;

        // 4. Verify earning >= $350 in Sandbox mode skips Campaign milestone bookkeeping
        //    (pending_upgrade_choices stays 0 and game remains Playing), and selecting an upgrade
        //    while paused in Sandbox mode preserves AppState::Paused.
        app.world_mut().resource_mut::<EnvironmentConfig>().mode = pd_core::GameMode::Sandbox;
        app.world_mut().send_event(DemolitionCashEvent {
            amount: 600,
            world_pos: Vec3::ZERO,
        });
        for _ in 0..3 {
            app.update();
        }
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Playing);
        assert_eq!(
            app.world()
                .resource::<pd_dozer::UpgradeProgression>()
                .pending_upgrade_choices,
            0
        );

        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Paused);
        for _ in 0..2 {
            app.update();
        }
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Paused);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Digit3);
        for _ in 0..3 {
            app.update();
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .clear();
        }
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Paused);
        assert!(
            (app.world().resource::<UpgradeMultipliers>().aux_ram - 1.35).abs() < 1e-5
        );

        // 5. Verify keyboard Enter in NewGameModal resets upgrades to 1.0x even when level changes
        app.world_mut().resource_mut::<EnvironmentConfig>().mode = pd_core::GameMode::Campaign;
        app.world_mut().resource_mut::<EnvironmentConfig>().level = pd_core::CampaignLevel::Village2;
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::NewGameModal);
        for _ in 0..2 {
            app.update();
        }
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::Enter);
        for _ in 0..3 {
            app.update();
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .clear();
        }
        assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Playing);
        let reset_upgrades = *app.world().resource::<UpgradeMultipliers>();
        assert!((reset_upgrades.ar400_edge - 1.0).abs() < 1e-5);
        assert!((reset_upgrades.charge_air_kit - 1.0).abs() < 1e-5);
        assert!((reset_upgrades.aux_ram - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_watchdog_uses_true_collider_overlap_not_broad_hit_radius() {
        AsyncComputeTaskPool::get_or_init(|| TaskPoolBuilder::new().num_threads(2).build());

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            StatesPlugin,
            TransformPlugin,
            HierarchyPlugin,
        ))
        .init_asset::<Image>()
        .init_asset::<TextureAtlasLayout>()
        .init_asset::<Mesh>()
        .init_asset::<ColorMaterial>()
        .init_resource::<ButtonInput<KeyCode>>()
        .add_plugins((
            PdCorePlugin,
            PdWorldPlugin,
            PdDozerPlugin,
            PdDestructionPlugin,
            PdAiPlugin,
            PdUiPlugin,
        ))
        .add_systems(
            Update,
            evaluate_dozer_structural_impacts_system.after(PdSystemSet::WorldTilemapSync),
        );

        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        for _ in 0..3 {
            app.update();
        }

        // Position the dozer at (0, 0) and a single building at (0, 80):
        // Center distance is 80 <= 95 (within broad demolition hit radius),
        // but the 46x30 and 108x76 colliders still have a 27-unit gap along Y.
        {
            let mut dozer_q = app
                .world_mut()
                .query_filtered::<(&mut WorldPose, &mut Position), With<PlayerVehicle>>();
            for (mut pose, mut pos) in dozer_q.iter_mut(app.world_mut()) {
                pose.pos = Vec2::ZERO;
                pose.heading_rad = 0.0;
                pos.0 = Vec2::ZERO;
            }

            let mut b_q = app
                .world_mut()
                .query_filtered::<(Entity, &mut WorldPose, &mut Position), With<ModularBuilding>>();
            let mut first = true;
            for (_e, mut pose, mut pos) in b_q.iter_mut(app.world_mut()) {
                if first {
                    pose.pos = Vec2::new(0.0, 80.0);
                    pos.0 = Vec2::new(0.0, 80.0);
                    first = false;
                } else {
                    pose.pos = Vec2::splat(5000.0);
                    pos.0 = Vec2::splat(5000.0);
                }
            }
        }

        app.update();
        let pen_separated = app.world().resource::<TrackInput>().penetration_depth;
        assert!(
            pen_separated <= 1e-5,
            "Separated colliders (27-unit gap) must report 0.0m penetration, got {pen_separated}"
        );

        // Now move the dozer to (0, 35) so the 46x30 and 108x76 colliders overlap by 8.0 units (0.50m) along Y.
        {
            let mut dozer_q = app
                .world_mut()
                .query_filtered::<(&mut WorldPose, &mut Position), With<PlayerVehicle>>();
            for (mut pose, mut pos) in dozer_q.iter_mut(app.world_mut()) {
                pose.pos = Vec2::new(0.0, 35.0);
                pose.heading_rad = 0.0;
                pos.0 = Vec2::new(0.0, 35.0);
            }
        }

        app.update();
        let track_in = app.world().resource::<TrackInput>();
        assert!(
            (track_in.penetration_depth - 0.50).abs() < 1e-3,
            "Expected 0.50m true collider overlap, got {}",
            track_in.penetration_depth
        );
        assert!(
            track_in.contact_normal.y < -0.99,
            "Expected recovery normal pointing along -Y away from building, got {:?}",
            track_in.contact_normal
        );
    }

    /// Epic #3 Verification Test:
    /// 1. Spatial Per-Cell Wall Breaching (#14): Breaching the south wall cells of a ranch building
    ///    removes solid collision from those breached cells so the dozer can drive inside the
    ///    interior room while the intact north/east/west walls continue blocking.
    /// 2. Merged Exterior Wall Spans & Sagging Roof Bays (#15): Losing `< 55%` of a roof bay's
    ///    supporting nodes transitions the bay `Intact -> Sagging -> Collapsed`, spawns pushable
    ///    roof remnants, and never pays a duplicate full-building collapse bonus.
    /// 3. Mass-Conserving Debris, `PileField` & `CollapsedSite` (#16): Crushing rubble transfers
    ///    exact mass into `PileField`, increases `pile_resistance_at`, and reaching `>= 90%`
    ///    settled building destruction spawns a `CollapsedSite` scar and awards the one-time
    ///    full-building collapse bonus.
    /// 4. 7 Destruction Profiles & Bounded Chained Explosions (#17): `Explosive` propane tanks
    ///    chain-detonate nearby explosive props up to `MAX_EXPLOSION_GENERATION` without infinite
    ///    recursion and damage adjacent building cells.
    #[test]
    fn test_epic_3_per_cell_breach_sagging_roofs_pile_field_and_7_profiles() {
        AsyncComputeTaskPool::get_or_init(|| TaskPoolBuilder::new().num_threads(2).build());

        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            StatesPlugin,
            TransformPlugin,
            HierarchyPlugin,
        ))
        .init_asset::<Image>()
        .init_asset::<TextureAtlasLayout>()
        .init_asset::<Mesh>()
        .init_asset::<ColorMaterial>()
        .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
            std::time::Duration::from_millis(4),
        ))
        .init_resource::<ButtonInput<KeyCode>>()
        .add_plugins((
            PdCorePlugin,
            PdWorldPlugin,
            PdDozerPlugin,
            PdDestructionPlugin,
            PdAiPlugin,
            PdUiPlugin,
        ))
        .add_systems(
            Update,
            (
                evaluate_dozer_structural_impacts_system,
                pd_destruction::poll_structural_collapse_tasks_system,
                pd_destruction::step_building_roofs_and_collapse_sites_system,
                pd_destruction::update_debris_lifecycle_system,
            )
                .chain()
                .after(PdSystemSet::WorldTilemapSync),
        );

        app.world_mut().resource_mut::<EnvironmentConfig>().mode = pd_core::GameMode::Sandbox;
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        for _ in 0..3 {
            app.update();
        }

        // 1. Locate first building at (-160, 120) and breach its south wall center cell (0, -2) -> world (-160, 90)
        let first_building = app
            .world_mut()
            .query_filtered::<Entity, With<ModularBuilding>>()
            .iter(app.world())
            .next()
            .unwrap_or(Entity::PLACEHOLDER);
        assert_ne!(first_building, Entity::PLACEHOLDER);

        // Position dozer with BladeDown facing +Y right at the south wall center cell (-160, 68)
        *app.world_mut().resource_mut::<BladeState>() = BladeState::BladeDown;
        {
            let mut dozer_q = app
                .world_mut()
                .query_filtered::<(&mut PlayerVehicle, &mut WorldPose, &mut Position), With<PlayerVehicle>>();
            for (mut veh, mut pose, mut pos) in dozer_q.iter_mut(app.world_mut()) {
                pose.pos = Vec2::new(-160.0, 68.0);
                pose.heading_rad = std::f32::consts::FRAC_PI_2;
                pos.0 = pose.pos;
                veh.vx = 0.0;
                veh.vy = 45.0;
                veh.pushing_force_multiplier = 2.0;
            }
        }
        app.update();

        // Verify south wall cells near (-160, 90) took localized damage/breached while north wall (gy == 2) stayed intact!
        {
            let reg = app.world().resource::<BuildingGraphRegistry>();
            if let Some(entry) = reg.buildings.get(&first_building) {
                let south_damaged = entry
                    .nodes
                    .values()
                    .any(|n| n.story_level == 0 && n.grid_y == -2 && (!n.intact || n.hp < n.max_hp));
                let north_all_intact = entry
                    .nodes
                    .values()
                    .filter(|n| n.story_level == 0 && n.grid_y == 2)
                    .all(|n| n.intact && (n.hp - n.max_hp).abs() < 1e-4);
                assert!(south_damaged, "South wall cells under the blade must take localized damage");
                assert!(north_all_intact, "Opposite north wall cells must remain untouched");
            }
        }

        // 2. Remove > 45% of supporting nodes for Center RoofBay (bay_id == 1) and verify Intact -> Sagging -> Collapsed!
        {
            let mut reg = app.world_mut().resource_mut::<BuildingGraphRegistry>();
            if let Some(entry) = reg.buildings.get_mut(&first_building) {
                let sup_ids: Vec<NodeIndex> = entry
                    .roof_bays
                    .iter()
                    .find(|b| b.bay_id == 1)
                    .map(|b| b.support_nodes.clone())
                    .unwrap_or_default();
                assert!(!sup_ids.is_empty());
                for id in sup_ids {
                    if let Some(n) = entry.nodes.get_mut(&id) {
                        n.intact = false;
                        n.hp = 0.0;
                    }
                }
            }
        }
        app.update();

        // Bay 1 should now be in RoofBayState::Sagging!
        {
            let reg = app.world().resource::<BuildingGraphRegistry>();
            if let Some(entry) = reg.buildings.get(&first_building) {
                let bay1 = entry.roof_bays.iter().find(|b| b.bay_id == 1);
                assert!(
                    bay1.is_some_and(|b| matches!(b.state, pd_destruction::RoofBayState::Sagging { .. })),
                    "Center roof bay must enter Sagging state when >45% of its support nodes are removed"
                );
                assert!(
                    !entry.collapse_bonus_paid,
                    "Partial roof bay sag/collapse must not trigger the full-building collapse bonus"
                );
            }
        }

        // Advance time so the sagging roof bay finishes hinging and collapses into pushable roof remnants
        app.world_mut().insert_resource(
            bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(25)),
        );
        for _ in 0..25 {
            app.update();
        }
        app.world_mut().insert_resource(
            bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(4)),
        );
        {
            let reg = app.world().resource::<BuildingGraphRegistry>();
            if let Some(entry) = reg.buildings.get(&first_building) {
                let bay1 = entry.roof_bays.iter().find(|b| b.bay_id == 1);
                assert!(
                    bay1.is_some_and(|b| b.state == pd_destruction::RoofBayState::Collapsed),
                    "Sagging roof bay must finish collapsing into RoofBayState::Collapsed"
                );
                assert!(
                    !entry.collapse_bonus_paid,
                    "Single roof bay collapse must not award full-building collapse bonus"
                );
            }
        }

        // 3. Test Explosive prop chaining (bounded by MAX_EXPLOSION_GENERATION = 1) and PileField mass conservation
        let epoch = *app.world().resource::<SessionEpoch>();
        let mass_before = app.world().resource::<PileField>().total_mass();
        let tank_a = app
            .world_mut()
            .spawn((
                SiteScoped { epoch },
                DestructibleProp::new(pd_destruction::PropArchetype::PropaneTank, false),
                WorldPose::from_xy(300.0, -60.0),
                Transform::from_xyz(300.0, -60.0, 0.0),
                GlobalTransform::default(),
            ))
            .id();
        let tank_b = app
            .world_mut()
            .spawn((
                SiteScoped { epoch },
                DestructibleProp::new(pd_destruction::PropArchetype::PropaneTank, false),
                WorldPose::from_xy(345.0, -60.0),
                Transform::from_xyz(345.0, -60.0, 0.0),
                GlobalTransform::default(),
            ))
            .id();

        // Move dozer onto tank_a at (300, -60) to trigger primary blast -> chains to tank_b at (345, -60)
        {
            let mut dozer_q = app
                .world_mut()
                .query_filtered::<(&mut WorldPose, &mut Position), With<PlayerVehicle>>();
            for (mut pose, mut pos) in dozer_q.iter_mut(app.world_mut()) {
                pose.pos = Vec2::new(285.0, -60.0);
                pose.heading_rad = 0.0;
                pos.0 = pose.pos;
            }
        }
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(16));
        app.update();

        assert!(
            app.world().get_entity(tank_a).is_err(),
            "Primary PropaneTank must detonate on blade impact"
        );
        assert!(
            app.world().get_entity(tank_b).is_err(),
            "Secondary PropaneTank within blast radius must chain-detonate"
        );
        let mass_after_tanks = app.world().resource::<PileField>().total_mass();
        assert!(
            mass_after_tanks > mass_before,
            "Destroyed props must deposit their conserved mass into PileField"
        );

        // 4. Destroy >= 90% of `first_building` nodes and verify it transitions into `CollapsedSite`
        //    and pays `collapse_bonus_paid` exactly once!
        {
            let mut reg = app.world_mut().resource_mut::<BuildingGraphRegistry>();
            if let Some(entry) = reg.buildings.get_mut(&first_building) {
                for node in entry.nodes.values_mut() {
                    node.intact = false;
                    node.hp = 0.0;
                }
            }
        }
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(16));
        app.update();

        {
            let reg = app.world().resource::<BuildingGraphRegistry>();
            if let Some(entry) = reg.buildings.get(&first_building) {
                assert!(entry.collapse_bonus_paid);
                assert!(entry.collapsed_site_spawned);
            }
        }
        assert!(
            app.world()
                .entity(first_building)
                .get::<pd_world::CollapsedSite>()
                .is_some(),
            "Building at >=90% settled destruction must gain CollapsedSite component"
        );
    }

    /// Regression for the Avian-while-paused leak: a moving body must not advance while
    /// `AppState` is anything other than `Playing`, and must resume once it is.
    #[test]
    fn test_physics_clock_frozen_outside_playing() {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            bevy::scene::ScenePlugin,
            StatesPlugin,
            TransformPlugin,
            HierarchyPlugin,
            bevy::diagnostic::DiagnosticsPlugin,
        ))
        .init_asset::<Mesh>()
        .add_plugins(PhysicsPlugins::default().with_length_unit(16.0))
            .insert_resource(Gravity(Vec2::ZERO))
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_millis(16),
            ))
            .init_state::<AppState>()
            .add_plugins(PhysicsStateGatePlugin);

        let body = app
            .world_mut()
            .spawn((
                RigidBody::Dynamic,
                Collider::circle(4.0),
                Position(Vec2::ZERO),
                LinearVelocity(Vec2::new(100.0, 0.0)),
            ))
            .id();

        let x_of = |app: &App| app.world().get::<Position>(body).map_or(f32::NAN, |p| p.0.x);

        // Title (default state): the body must stay put.
        for _ in 0..20 {
            app.update();
        }
        assert!(x_of(&app).abs() < 1e-4, "body moved during Title: x = {}", x_of(&app));

        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Playing);
        for _ in 0..20 {
            app.update();
        }
        let after_play = x_of(&app);
        assert!(after_play > 1.0, "body must move while Playing: x = {after_play}");

        // The very first paused tick must not step either (Avian 0.2 checks the stale delta).
        app.world_mut().resource_mut::<NextState<AppState>>().set(AppState::Paused);
        for _ in 0..30 {
            app.update();
            assert!(
                x_of(&app).to_bits() == after_play.to_bits(),
                "body drifted while Paused: {after_play} -> {}",
                x_of(&app)
            );
        }
    }
}
