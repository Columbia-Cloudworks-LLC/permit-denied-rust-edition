use avian3d::prelude::*;
use bevy::prelude::*;

use crate::building::RubbleState;

#[derive(Resource)]
pub struct DebrisStats {
    pub active_dynamic: usize,
    pub max_cap: usize,
    pub total_settled: usize,
}

impl Default for DebrisStats {
    fn default() -> Self {
        Self {
            active_dynamic: 0,
            max_cap: 400,
            total_settled: 0,
        }
    }
}

// --- PostUpdate: debris_lifecycle_system ---

pub fn debris_lifecycle_system(
    time: Res<Time>,
    mut commands: Commands,
    mut stats: ResMut<DebrisStats>,
    mut debris_query: Query<(
        Entity,
        &mut RubbleState,
        &LinearVelocity,
        &AngularVelocity,
        &Transform,
        &RigidBody,
    )>,
) {
    let dt = time.delta_secs();

    let mut dynamic_entities: Vec<(Entity, f32)> = Vec::new();
    let mut settled_count = 0;

    for (entity, mut rubble, lin_vel, ang_vel, transform, rb) in debris_query.iter_mut() {
        if *rb != RigidBody::Dynamic {
            settled_count += 1;
            continue;
        }

        let speed = lin_vel.0.length();
        let ang_speed = ang_vel.0.length();

        // Aggressive Sleep Threshold: lin_vel < 0.15 m/s and ang_vel < 0.1 rad/s
        let is_stationary = speed < 0.15 && ang_speed < 0.1;

        if is_stationary {
            rubble.stationary_timer += dt;
        } else {
            rubble.stationary_timer = (rubble.stationary_timer - dt * 2.0).max(0.0);
        }

        // Chunk Despawn / Freeze:
        // Debris resting below floor level (Y < -0.2) or stationary for > 10.0 s
        let below_floor = transform.translation.y < -0.2;
        let expired = rubble.stationary_timer > 10.0;

        if below_floor || expired {
            // Drop dynamic rigid body and bake into static world chunk
            commands
                .entity(entity)
                .remove::<RigidBody>()
                .insert(RigidBody::Static);

            rubble.can_bake = true;
            settled_count += 1;
            continue;
        }

        dynamic_entities.push((entity, rubble.stationary_timer));
    }

    stats.active_dynamic = dynamic_entities.len();
    stats.total_settled = settled_count;

    // Enforce Dynamic Debris Cap: Max 400 active dynamic debris entities
    if dynamic_entities.len() > stats.max_cap {
        // Sort by stationary timer descending (freeze the most settled chunks first)
        dynamic_entities.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let overflow = dynamic_entities.len() - stats.max_cap;
        for (entity_to_freeze, _) in dynamic_entities.into_iter().take(overflow) {
            commands
                .entity(entity_to_freeze)
                .remove::<RigidBody>()
                .insert(RigidBody::Static);
        }
    }
}
