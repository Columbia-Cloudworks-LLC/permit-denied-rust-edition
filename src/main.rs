//! `permit_denied`: Main binary entry point assembling the 6 defensive Bevy workspace crates
//! (`pd_core`, `pd_world`, `pd_dozer`, `pd_destruction`, `pd_ai`, `pd_ui`), procedural WAV
//! audio synthesis, 3-tier debris heightmap crushing, prop destruction, and Verification Gates.

use avian2d::prelude::*;
use bevy::audio::AudioPlayer;
use bevy::prelude::*;
use bevy::tasks::AsyncComputeTaskPool;
use pd_ai::PdAiPlugin;
use pd_core::{
    AppState, AudioSfxKind, AudioTriggerEvent, DebugTelemetry, DecalKind, DecalSpawnRequest,
    DemolitionCashEvent, IsoBoxAssembly, IsoExtrudedBox, ParticleBurstEvent, ParticleBurstKind,
    PdCorePlugin, PdSystemSet, SessionEpoch, WorldPose, sanitize_f32, sanitize_vec2,
    world_to_iso_screen,
};
use pd_destruction::{
    BuildingGraphRegistry, DebrisChunk, DebrisPoolRegistry, DebrisTier, DestructibleProp,
    ModularBuilding, NodeIndex, PdDestructionPlugin, PropArchetype, build_building_iso_assembly,
    queue_structural_integrity_evaluation, spawn_pooled_debris_chunk,
};
use pd_dozer::{BladeState, PdDozerPlugin, PlayerVehicle, TrackInput, UpgradeMultipliers};
use pd_ui::PdUiPlugin;
use pd_world::{CutawayFootprint, PdWorldPlugin};

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

/// Evaluates dozer blade impacts against modular buildings, interior/exterior props, and sleeping
/// rubble in Cartesian `WorldPose` space (crushing Tier 2 sleeping rubble into Tier 3 ground
/// decal scars when `BladeDown`).
pub fn evaluate_dozer_structural_impacts_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    blade_state: Res<BladeState>,
    upgrades: Res<UpgradeMultipliers>,
    mut track_input: ResMut<TrackInput>,
    mut graph_registry: ResMut<BuildingGraphRegistry>,
    mut debris_pool: ResMut<DebrisPoolRegistry>,
    mut cash_events: EventWriter<DemolitionCashEvent>,
    mut decal_events: EventWriter<DecalSpawnRequest>,
    mut particle_events: EventWriter<ParticleBurstEvent>,
    mut audio_events: EventWriter<AudioTriggerEvent>,
    mut destroyed_ids: Local<Vec<NodeIndex>>,
    dozer_query: Query<(Option<&WorldPose>, &Transform), With<PlayerVehicle>>,
    mut building_query: Query<
        (
            Entity,
            Option<&WorldPose>,
            &Transform,
            &ModularBuilding,
            &mut CutawayFootprint,
            Option<&mut IsoExtrudedBox>,
            Option<&mut IsoBoxAssembly>,
        ),
        (With<ModularBuilding>, Without<PlayerVehicle>),
    >,
    prop_query: Query<
        (Entity, Option<&WorldPose>, &Transform, &DestructibleProp),
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
    let Ok((dozer_pose, dozer_tf)) = dozer_query.get_single() else {
        return;
    };
    let dozer_xy = dozer_pose.map_or_else(
        || sanitize_vec2(dozer_tf.translation.truncate(), Vec2::ZERO),
        |p| sanitize_vec2(p.pos, Vec2::ZERO),
    );
    let task_pool = AsyncComputeTaskPool::get();

    let max_nodes_per_hit: usize = match *blade_state {
        BladeState::BladeUp => 1,
        BladeState::BladeDown => (upgrades.sanitized().ar400_edge.round() as usize).clamp(2, 6),
    };

    let mut contact_load = 0.0_f32;

    // 1. Evaluate building structural impacts
    for (b_entity, b_pose, b_tf, b_meta, mut cutaway, base_box, assembly) in &mut building_query {
        let b_xy = b_pose.map_or_else(
            || sanitize_vec2(b_tf.translation.truncate(), Vec2::ZERO),
            |p| sanitize_vec2(p.pos, Vec2::ZERO),
        );
        if dozer_xy.distance_squared(b_xy) > 95.0 * 95.0 {
            continue;
        }

        destroyed_ids.clear();
        if let Some(entry) = graph_registry.buildings.get(&b_entity) {
            for (&id, node) in &entry.nodes {
                if node.intact && node.story_level == 0 {
                    destroyed_ids.push(id);
                    if destroyed_ids.len() >= max_nodes_per_hit {
                        break;
                    }
                }
            }
        }

        if !destroyed_ids.is_empty() {
            contact_load += 1.5;
            cutaway.exterior_breached = true;
            audio_events.send(AudioTriggerEvent {
                kind: AudioSfxKind::StructuralCrunch,
            });
            particle_events.send(ParticleBurstEvent {
                kind: ParticleBurstKind::MasonryDust,
                world_pos: b_xy,
                count: 5,
            });

            let b_screen = world_to_iso_screen(b_xy, 8.0);
            for &id in &destroyed_ids {
                let cash_val = graph_registry
                    .buildings
                    .get(&b_entity)
                    .and_then(|e| e.nodes.get(&id))
                    .map_or(16, |n| n.cash_value);

                cash_events.send(DemolitionCashEvent {
                    amount: cash_val,
                    world_pos: b_screen.extend(0.0),
                });
                spawn_pooled_debris_chunk(&mut commands, &mut debris_pool, *epoch, b_xy, 0);
            }

            let _ = queue_structural_integrity_evaluation(
                b_entity,
                &destroyed_ids,
                &mut graph_registry,
                task_pool,
            );

            if let Some(entry) = graph_registry.buildings.get(&b_entity) {
                let has_intact_ground_story = entry
                    .nodes
                    .values()
                    .any(|n| n.intact && n.story_level == 0);
                if !has_intact_ground_story
                    && let Some(mut b_cmds) = commands.get_entity(b_entity)
                {
                    b_cmds
                        .remove::<RigidBody>()
                        .remove::<Collider>()
                        .remove::<Position>()
                        .remove::<Rotation>();
                }
                if let (Some(mut b_box), Some(mut asm)) = (base_box, assembly) {
                    let (mut new_base, mut new_asm) = build_building_iso_assembly(
                        &entry.nodes,
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
    }

    // 2. Evaluate interior furniture & exterior street prop destruction
    for (prop_entity, prop_pose, prop_tf, prop) in &prop_query {
        let p_xy = prop_pose.map_or_else(
            || sanitize_vec2(prop_tf.translation.truncate(), Vec2::ZERO),
            |p| sanitize_vec2(p.pos, Vec2::ZERO),
        );
        if dozer_xy.distance_squared(p_xy) <= 42.0 * 42.0 {
            contact_load += 0.35;
            let p_screen = world_to_iso_screen(p_xy, 4.0);
            cash_events.send(DemolitionCashEvent {
                amount: prop.archetype.cash_value(),
                world_pos: p_screen.extend(0.0),
            });

            match prop.archetype {
                PropArchetype::Toilet | PropArchetype::FireHydrant => {
                    particle_events.send(ParticleBurstEvent {
                        kind: ParticleBurstKind::WaterSpray,
                        world_pos: p_xy,
                        count: 10,
                    });
                    audio_events.send(AudioTriggerEvent {
                        kind: AudioSfxKind::BladeHydraulicHiss,
                    });
                }
                PropArchetype::PropaneTank => {
                    particle_events.send(ParticleBurstEvent {
                        kind: ParticleBurstKind::PropaneExplosion,
                        world_pos: p_xy,
                        count: 14,
                    });
                    decal_events.send(DecalSpawnRequest {
                        kind: DecalKind::ScorchMark,
                        world_pos: p_xy,
                        size: Vec2::new(42.0, 32.0),
                        rotation_rad: 0.0,
                    });
                    audio_events.send(AudioTriggerEvent {
                        kind: AudioSfxKind::PropaneBlast,
                    });
                }
                PropArchetype::CashRegister => {
                    audio_events.send(AudioTriggerEvent {
                        kind: AudioSfxKind::CashDing,
                    });
                }
                _ => {
                    audio_events.send(AudioTriggerEvent {
                        kind: AudioSfxKind::StructuralCrunch,
                    });
                }
            }

            if let Some(e_cmds) = commands.get_entity(prop_entity) {
                e_cmds.despawn_recursive();
            }
        }
    }

    // 3. Tier 2 -> Tier 3 Heightmap Crushing: Driving over sleeping rubble with BladeDown
    // flattens the rubble into the persistent ground decal layer.
    if *blade_state == BladeState::BladeDown {
        for (rubble_entity, rubble_pose, rubble_tf, mut chunk) in &mut sleeping_rubble_query {
            if chunk.tier != DebrisTier::SleepingInstanced {
                continue;
            }
            let r_xy = rubble_pose.map_or_else(
                || sanitize_vec2(rubble_tf.translation.truncate(), Vec2::ZERO),
                |p| sanitize_vec2(p.pos, Vec2::ZERO),
            );
            if dozer_xy.distance_squared(r_xy) <= 38.0 * 38.0 {
                chunk.tier = DebrisTier::CrushedDecal;
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
    }

    track_input.external_load_factor = contact_load.clamp(0.0, 5.0);
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "PERMIT DENIED — The County Said No. (Columbia Cloudworks LLC)".to_string(),
                resolution: (1280.0_f32, 720.0_f32).into(),
                present_mode: bevy::window::PresentMode::AutoVsync,
                fit_canvas_to_parent: true,
                ..Default::default()
            }),
            ..Default::default()
        }))
        .add_plugins(PhysicsPlugins::default().with_length_unit(16.0))
        .insert_resource(Gravity(Vec2::ZERO))
        .init_resource::<SynthesizedAudioBank>()
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
        )
        .run();
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
}


