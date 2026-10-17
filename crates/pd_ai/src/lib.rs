//! `pd_ai`: Spatial hash crowd AI, 8-directional animated pedestrians (`Stroll`, `Panic`, `Flee`),
//! and ambient/emergency traffic vehicles with spline-to-physics transition, oil slick decals,
//! and rubble halting.
//!
//! Depends only on `pd_core` and `pd_world`.

use avian2d::prelude::*;
use bevy::prelude::*;
use pd_core::{
    ActiveCollapseZones, AppState, AudioSfxKind, AudioTriggerEvent, CashPopupPool, CollapseZone,
    DebugTelemetry, DecalKind, DecalSpawnRequest, DemolitionCashEvent, EnvironmentConfig, GameMode,
    PdSystemSet, SessionEpoch, SiteScoped, sanitize_dt, sanitize_f32, sanitize_vec2,
};
use pd_world::{RoadSplineGraph, SpatialHashGrid, compute_stable_iso_z};
use serde::{Deserialize, Serialize};

/// Threat detection radius around the dozer and active building collapses.
pub const THREAT_RADIUS: f32 = 160.0;

/// Direct impact radius within which traffic vehicles transition from spline-followers to physics bodies.
pub const VEHICLE_IMPACT_RADIUS: f32 = 54.0;

/// Maximum neighbor avoidance checks per fleeing pedestrian (prevents $O(N^2)$ crowd stalls).
pub const MAX_NEIGHBOR_CHECKS: usize = 8;

/// Maximum knockback speed applied when a spline vehicle transitions to a dynamic physics body.
pub const MAX_VEHICLE_KNOCKBACK_SPEED: f32 = 220.0;

/// Pedestrian flee speed (world units per second).
pub const PEDESTRIAN_PANIC_SPEED: f32 = 72.0;

/// 8-directional isometric billboard facing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Facing8Way {
    #[default]
    South,
    SouthEast,
    East,
    NorthEast,
    North,
    NorthWest,
    West,
    SouthWest,
}

impl Facing8Way {
    #[must_use]
    pub fn from_velocity(vel: Vec2) -> Self {
        let safe = sanitize_vec2(vel, Vec2::ZERO);
        if safe.length_squared() < 1e-4 {
            return Self::South;
        }
        let angle = safe.y.atan2(safe.x);
        let octant = ((angle + std::f32::consts::PI) / (std::f32::consts::FRAC_PI_4)).round() as i32;
        match octant.rem_euclid(8) {
            0 => Self::West,
            1 => Self::SouthWest,
            2 => Self::South,
            3 => Self::SouthEast,
            4 => Self::East,
            5 => Self::NorthEast,
            6 => Self::North,
            _ => Self::NorthWest,
        }
    }
}

/// Ambient and reactive vehicle archetypes per Section 4 of the PRD.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VehicleArchetype {
    Sedan,
    PickupTruck,
    DeliveryVan,
    CityBus,
    CountyInspectorTruck,
    PoliceCruiser,
    SwatVan,
}

impl VehicleArchetype {
    #[must_use]
    pub const fn cash_value(self) -> u32 {
        match self {
            Self::CountyInspectorTruck | Self::PoliceCruiser | Self::SwatVan | Self::CityBus => 89,
            Self::Sedan | Self::PickupTruck | Self::DeliveryVan => 71,
        }
    }

    #[must_use]
    pub const fn base_color(self) -> Color {
        match self {
            Self::Sedan => Color::srgb(0.24, 0.48, 0.76),
            Self::PickupTruck => Color::srgb(0.68, 0.24, 0.20),
            Self::DeliveryVan => Color::srgb(0.88, 0.88, 0.90),
            Self::CityBus => Color::srgb(0.92, 0.72, 0.18),
            Self::CountyInspectorTruck => Color::srgb(0.95, 0.58, 0.12),
            Self::PoliceCruiser => Color::srgb(0.16, 0.24, 0.52),
            Self::SwatVan => Color::srgb(0.18, 0.20, 0.24),
        }
    }
}

/// Marker component for vehicles currently following the road lane spline cleanly.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SplineFollower {
    pub target_node_index: usize,
    pub reversing: bool,
}

/// Marker component inserted when a struck vehicle switches from spline-follower to full physics body.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct DynamicPhysicsVehicle;

/// Unified agent state machine for pedestrians and traffic vehicles queried via `SpatialHashGrid`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AgentState {
    PedestrianStroll {
        velocity: Vec2,
        facing: Facing8Way,
    },
    PedestrianPanic {
        escape_velocity: Vec2,
        facing: Facing8Way,
    },
    TrafficSpline {
        archetype: VehicleArchetype,
        target_node: usize,
        speed: f32,
        halted_by_rubble: bool,
    },
    TrafficDynamicPhysics {
        archetype: VehicleArchetype,
        knockback_velocity: Vec2,
    },
}

/// Computes a deterministic, non-zero unit escape vector from an `Entity` ID when the threat
/// is directly on top of the agent (`distance == 0.0`), preventing division-by-zero (`NaN`).
#[must_use]
pub fn deterministic_escape_vector(entity: Entity) -> Vec2 {
    let idx = entity.index();
    let angle = (idx % 16) as f32 * (std::f32::consts::TAU / 16.0);
    let candidate = Vec2::new(angle.cos(), angle.sin());
    sanitize_vec2(candidate, Vec2::X)
}

/// Computes a safe unit vector pointing from `threat_pos` toward `agent_pos`, falling back
/// to `deterministic_escape_vector` when `distance_squared < 1e-6`.
#[must_use]
pub fn safe_flee_direction(agent_pos: Vec2, threat_pos: Vec2, entity: Entity) -> Vec2 {
    let safe_agent = sanitize_vec2(agent_pos, Vec2::ZERO);
    let safe_threat = sanitize_vec2(threat_pos, Vec2::ZERO);
    let delta = safe_agent - safe_threat;
    let len_sq = delta.length_squared();
    if !len_sq.is_finite() || len_sq < 1e-6 {
        deterministic_escape_vector(entity)
    } else {
        let inv_len = 1.0 / len_sq.sqrt();
        sanitize_vec2(delta * inv_len, deterministic_escape_vector(entity))
    }
}

/// Contract Function (Module D): Queries the spatial hash grid around the dozer and active
/// building collapse zones to transition pedestrians into `Panic/Flee` states and switch struck
/// traffic vehicles from spline-followers to dynamic physics bodies.
pub fn update_crowd_and_traffic_spatial_grid(
    dozer_transform: &Transform,
    active_collapses: &[CollapseZone],
    spatial_grid: &mut SpatialHashGrid,
    agents: &mut Query<(Entity, &mut AgentState, &Transform)>,
) {
    spatial_grid.clear();
    for (entity, _, transform) in agents.iter() {
        let pos = sanitize_vec2(transform.translation.truncate(), Vec2::ZERO);
        spatial_grid.insert(entity, pos);
    }

    let dozer_pos = sanitize_vec2(dozer_transform.translation.truncate(), Vec2::ZERO);
    let mut nearby_candidates: Vec<(Entity, Vec2)> = Vec::with_capacity(64);
    let mut neighbor_scratch: Vec<(Entity, Vec2)> = Vec::with_capacity(16);

    // 1. Query spatial_grid around dozer_transform within THREAT_RADIUS.
    spatial_grid.query_radius(dozer_pos, THREAT_RADIUS, &mut nearby_candidates);

    // Also query around each active building collapse zone.
    for zone in active_collapses {
        let zone_center = sanitize_vec2(zone.center, Vec2::ZERO);
        let zone_radius = sanitize_f32(zone.radius, THREAT_RADIUS).max(THREAT_RADIUS);
        spatial_grid.query_radius(zone_center, zone_radius, &mut nearby_candidates);
    }

    // Deduplicate candidate entities while preserving order.
    nearby_candidates.sort_by_key(|(e, _)| e.index());
    nearby_candidates.dedup_by_key(|(e, _)| *e);

    for (entity, agent_pos) in nearby_candidates {
        // Find closest threat (dozer or active collapse center).
        let mut closest_threat = dozer_pos;
        let mut closest_dist_sq = agent_pos.distance_squared(dozer_pos);
        let mut struck_by_collapse = false;

        for zone in active_collapses {
            let zc = sanitize_vec2(zone.center, Vec2::ZERO);
            let d_sq = agent_pos.distance_squared(zc);
            if d_sq < closest_dist_sq {
                closest_dist_sq = d_sq;
                closest_threat = zc;
            }
            let strike_r = sanitize_f32(zone.radius, 80.0) * 0.65;
            if d_sq <= strike_r * strike_r {
                struck_by_collapse = true;
            }
        }

        // Local avoidance capped at MAX_NEIGHBOR_CHECKS (8 neighbors).
        neighbor_scratch.clear();
        spatial_grid.query_radius(agent_pos, 28.0, &mut neighbor_scratch);
        let mut separation = Vec2::ZERO;
        for &(other_entity, other_pos) in neighbor_scratch.iter().take(MAX_NEIGHBOR_CHECKS) {
            if other_entity != entity {
                let away = safe_flee_direction(agent_pos, other_pos, entity);
                separation += away * 0.25;
            }
        }

        let escape_dir =
            (safe_flee_direction(agent_pos, closest_threat, entity) + separation).normalize_or_zero();
        let final_dir = if escape_dir.length_squared() < 1e-4 {
            deterministic_escape_vector(entity)
        } else {
            escape_dir
        };

        if let Ok((_, mut state, _)) = agents.get_mut(entity) {
            match *state {
                AgentState::PedestrianStroll { .. } | AgentState::PedestrianPanic { .. } => {
                    let vel = sanitize_vec2(
                        final_dir * PEDESTRIAN_PANIC_SPEED,
                        deterministic_escape_vector(entity) * PEDESTRIAN_PANIC_SPEED,
                    );
                    *state = AgentState::PedestrianPanic {
                        escape_velocity: vel,
                        facing: Facing8Way::from_velocity(vel),
                    };
                }
                AgentState::TrafficSpline { archetype, .. } => {
                    let struck_by_dozer =
                        closest_dist_sq <= VEHICLE_IMPACT_RADIUS * VEHICLE_IMPACT_RADIUS;
                    if struck_by_dozer || struck_by_collapse {
                        let knockback = sanitize_vec2(
                            final_dir * MAX_VEHICLE_KNOCKBACK_SPEED,
                            Vec2::new(MAX_VEHICLE_KNOCKBACK_SPEED, 0.0),
                        );
                        *state = AgentState::TrafficDynamicPhysics {
                            archetype,
                            knockback_velocity: knockback,
                        };
                    }
                }
                AgentState::TrafficDynamicPhysics { .. } => {}
            }
        }
    }
}

/// System wrapper invoking `update_crowd_and_traffic_spatial_grid` and applying agent velocities,
/// road spline progression, oil slick decals, and spline-to-physics component transitions.
pub fn fixed_crowd_and_traffic_ai_system(
    mut commands: Commands,
    time: Res<Time>,
    popup_pool: Res<CashPopupPool>,
    collapse_zones: Res<ActiveCollapseZones>,
    road_graph: Res<RoadSplineGraph>,
    mut spatial_grid: ResMut<SpatialHashGrid>,
    mut telemetry: ResMut<DebugTelemetry>,
    mut cash_events: EventWriter<DemolitionCashEvent>,
    mut decal_events: EventWriter<DecalSpawnRequest>,
    mut audio_events: EventWriter<AudioTriggerEvent>,
    mut agent_queries: ParamSet<(
        Query<(Entity, &mut AgentState, &Transform)>,
        Query<(
            Entity,
            &mut AgentState,
            &mut Transform,
            Option<&mut Sprite>,
            Option<&SplineFollower>,
            Option<&mut LinearVelocity>,
        )>,
    )>,
) {
    let Ok(dt) = sanitize_dt(time.delta_secs()) else {
        return;
    };

    let dozer_transform = Transform::from_translation(popup_pool.fallback_dozer_pos);
    {
        let mut p0 = agent_queries.p0();
        update_crowd_and_traffic_spatial_grid(
            &dozer_transform,
            &collapse_zones.zones,
            &mut spatial_grid,
            &mut p0,
        );
    }

    let mut panicked_count: usize = 0;

    // Apply locomotion and spline-to-physics transitions.
    for (entity, mut state, mut transform, sprite, spline_follower, lin_vel) in
        &mut agent_queries.p1()
    {
        match *state {
            AgentState::PedestrianStroll { velocity, .. } => {
                let v = sanitize_vec2(velocity, Vec2::ZERO);
                transform.translation.x =
                    sanitize_f32(transform.translation.x + v.x * dt, transform.translation.x);
                transform.translation.y =
                    sanitize_f32(transform.translation.y + v.y * dt, transform.translation.y);
            }
            AgentState::PedestrianPanic {
                escape_velocity, ..
            } => {
                panicked_count = panicked_count.saturating_add(1);
                let v = sanitize_vec2(escape_velocity, Vec2::ZERO);
                transform.translation.x =
                    sanitize_f32(transform.translation.x + v.x * dt, transform.translation.x);
                transform.translation.y =
                    sanitize_f32(transform.translation.y + v.y * dt, transform.translation.y);
                if let Some(mut sp) = sprite {
                    sp.color = Color::srgb(0.98, 0.34, 0.22);
                }
            }
            AgentState::TrafficSpline {
                ref mut target_node,
                speed,
                ref mut halted_by_rubble,
                ..
            } => {
                if let Some(node) = road_graph.nodes.get(*target_node) {
                    if node.blocked_by_rubble {
                        *halted_by_rubble = true;
                    } else {
                        *halted_by_rubble = false;
                        let current_xy = transform.translation.truncate();
                        let to_target = node.position - current_xy;
                        if to_target.length_squared() < 225.0 {
                            if let Some(next_idx) = node.next_node {
                                *target_node = next_idx;
                            }
                        } else {
                            let step =
                                to_target.normalize_or_zero() * sanitize_f32(speed, 60.0) * dt;
                            transform.translation.x = sanitize_f32(
                                transform.translation.x + step.x,
                                transform.translation.x,
                            );
                            transform.translation.y = sanitize_f32(
                                transform.translation.y + step.y,
                                transform.translation.y,
                            );
                        }
                    }
                }
            }
            AgentState::TrafficDynamicPhysics {
                archetype,
                knockback_velocity,
            } => {
                let clamped_vel = sanitize_vec2(knockback_velocity, Vec2::ZERO)
                    .clamp_length_max(MAX_VEHICLE_KNOCKBACK_SPEED);
                if spline_follower.is_some() {
                    let pos_xy = transform.translation.truncate();
                    cash_events.send(DemolitionCashEvent {
                        amount: archetype.cash_value(),
                        world_pos: pos_xy.extend(0.0),
                    });
                    decal_events.send(DecalSpawnRequest {
                        kind: DecalKind::OilSlick,
                        world_pos: pos_xy,
                        size: Vec2::new(26.0, 18.0),
                        rotation_rad: 0.3,
                    });
                    audio_events.send(AudioTriggerEvent {
                        kind: AudioSfxKind::StructuralCrunch,
                    });
                    if let Some(mut sp) = sprite {
                        sp.color = Color::srgb(0.32, 0.33, 0.36);
                    }
                    if let Some(mut e_cmds) = commands.get_entity(entity) {
                        e_cmds.remove::<SplineFollower>().insert((
                            DynamicPhysicsVehicle,
                            RigidBody::Dynamic,
                            Collider::rectangle(34.0, 18.0),
                            LinearVelocity(clamped_vel),
                            AngularVelocity(1.8),
                        ));
                    }
                } else if let Some(mut lv) = lin_vel {
                    lv.0 = sanitize_vec2(lv.0 * 0.96, Vec2::ZERO);
                }
            }
        }
    }

    telemetry.panicked_pedestrians = panicked_count;
}

/// Spawns initial ambient pedestrians and road vehicles for the current `SessionEpoch`.
pub fn ensure_site_crowd_and_traffic_spawned_system(
    mut commands: Commands,
    epoch: Res<SessionEpoch>,
    env_config: Res<EnvironmentConfig>,
    road_graph: Res<RoadSplineGraph>,
    existing_agents: Query<Entity, With<AgentState>>,
) {
    if existing_agents.iter().next().is_some() {
        return;
    }

    // Spawn 10 ambient pedestrians strolling on sidewalks.
    for i in 0..10_u8 {
        let angle = f32::from(i) * (std::f32::consts::TAU / 10.0);
        let pos = Vec2::new(angle.cos() * 190.0, angle.sin() * 140.0);
        let stroll_vel = Vec2::new(-angle.sin() * 18.0, angle.cos() * 18.0);

        commands.spawn((
            Name::new(format!("Pedestrian #{i}")),
            SiteScoped { epoch: *epoch },
            AgentState::PedestrianStroll {
                velocity: stroll_vel,
                facing: Facing8Way::from_velocity(stroll_vel),
            },
            Sprite {
                color: Color::srgb(0.92, 0.78, 0.32),
                custom_size: Some(Vec2::new(10.0, 16.0)),
                ..Default::default()
            },
            Transform::from_xyz(pos.x, pos.y, compute_stable_iso_z(pos.x, pos.y, 4.0, 8.0)),
            GlobalTransform::default(),
        ));
    }

    // Spawn 5 vehicles along the road spline graph.
    let archetypes = if env_config.mode == GameMode::Campaign {
        [
            VehicleArchetype::Sedan,
            VehicleArchetype::DeliveryVan,
            VehicleArchetype::CountyInspectorTruck,
            VehicleArchetype::PoliceCruiser,
            VehicleArchetype::SwatVan,
        ]
    } else {
        [
            VehicleArchetype::Sedan,
            VehicleArchetype::PickupTruck,
            VehicleArchetype::DeliveryVan,
            VehicleArchetype::CityBus,
            VehicleArchetype::Sedan,
        ]
    };

    for (idx, &archetype) in archetypes.iter().enumerate() {
        let spawn_xy = road_graph
            .nodes
            .get(idx)
            .map_or(Vec2::new(-220.0, -220.0), |n| n.position);
        let next_idx = road_graph
            .nodes
            .get(idx)
            .and_then(|n| n.next_node)
            .unwrap_or(0);

        commands.spawn((
            Name::new(format!("Traffic Vehicle #{idx} ({archetype:?})")),
            SiteScoped { epoch: *epoch },
            SplineFollower {
                target_node_index: next_idx,
                reversing: false,
            },
            AgentState::TrafficSpline {
                archetype,
                target_node: next_idx,
                speed: 65.0,
                halted_by_rubble: false,
            },
            Sprite {
                color: archetype.base_color(),
                custom_size: Some(Vec2::new(34.0, 20.0)),
                ..Default::default()
            },
            Transform::from_xyz(
                spawn_xy.x,
                spawn_xy.y,
                compute_stable_iso_z(spawn_xy.x, spawn_xy.y, 8.0, 9.0),
            ),
            GlobalTransform::default(),
        ));
    }
}

/// Bevy plugin for `pd_ai`.
pub struct PdAiPlugin;

impl Plugin for PdAiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            ensure_site_crowd_and_traffic_spawned_system
                .run_if(in_state(AppState::Playing))
                .after(PdSystemSet::WorldTilemapSync)
                .before(PdSystemSet::HudInterpolation),
        )
        .add_systems(
            FixedUpdate,
            fixed_crowd_and_traffic_ai_system
                .run_if(in_state(AppState::Playing))
                .in_set(PdSystemSet::CrowdAndTrafficAi),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::SystemState;

    #[test]
    fn test_zero_distance_threat_produces_finite_escape_vector() {
        let mut world = World::new();
        let e1 = world
            .spawn((
                AgentState::PedestrianStroll {
                    velocity: Vec2::ZERO,
                    facing: Facing8Way::South,
                },
                Transform::from_xyz(50.0, 50.0, 0.0),
            ))
            .id();

        let e2 = world
            .spawn((
                AgentState::TrafficSpline {
                    archetype: VehicleArchetype::CountyInspectorTruck,
                    target_node: 0,
                    speed: 60.0,
                    halted_by_rubble: false,
                },
                Transform::from_xyz(50.0, 50.0, 0.0),
            ))
            .id();

        // Dozer is at exact same position (50.0, 50.0) -> distance == 0.0
        let dozer_transform = Transform::from_xyz(50.0, 50.0, 0.0);
        let mut grid = SpatialHashGrid::default();
        let mut sys_state: SystemState<Query<(Entity, &mut AgentState, &Transform)>> =
            SystemState::new(&mut world);
        {
            let mut query = sys_state.get_mut(&mut world);
            update_crowd_and_traffic_spatial_grid(&dozer_transform, &[], &mut grid, &mut query);
        }

        if let Some(state1) = world.get::<AgentState>(e1) {
            let is_panic = matches!(
                *state1,
                AgentState::PedestrianPanic {
                    escape_velocity,
                    ..
                } if escape_velocity.is_finite() && escape_velocity.length() > 1.0
            );
            assert!(is_panic);
        }

        if let Some(state2) = world.get::<AgentState>(e2) {
            let is_dynamic = matches!(
                *state2,
                AgentState::TrafficDynamicPhysics {
                    knockback_velocity,
                    ..
                } if knockback_velocity.is_finite() && knockback_velocity.length() > 1.0
            );
            assert!(is_dynamic);
        }
    }
}
