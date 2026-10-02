use avian3d::prelude::*;
use bevy::prelude::*;

use crate::building::{RubbleState, StructuralCell};
use crate::camera::CameraShake;
use crate::dozer::{ArmoredDozer, BladeColliderMarker};

// --- Update: collision_impulse_check_system ---

pub fn collision_impulse_check_system(
    mut commands: Commands,
    mut collision_events: EventReader<CollisionStarted>,
    collisions: Res<Collisions>,
    mut dozer_query: Query<(
        Entity,
        &Transform,
        &mut LinearVelocity,
        &mut AngularVelocity,
        &Mass,
    ), With<ArmoredDozer>>,
    blade_query: Query<(Entity, &Parent), With<BladeColliderMarker>>,
    mut cell_query: Query<(
        Entity,
        &mut StructuralCell,
        &Transform,
        &RigidBody,
    )>,
    mut camera_shake: ResMut<CameraShake>,
) {
    let Ok((dozer_entity, dozer_tf, mut dozer_lin_vel, _dozer_ang_vel, dozer_mass)) =
        dozer_query.get_single_mut()
    else {
        return;
    };

    for event in collision_events.read() {
        let (e1, e2) = (event.0, event.1);

        // Determine if one of the entities is the dozer or its blade
        let is_dozer_e1 = e1 == dozer_entity
            || blade_query.get(e1).map(|(_, p)| p.get() == dozer_entity).unwrap_or(false);
        let is_dozer_e2 = e2 == dozer_entity
            || blade_query.get(e2).map(|(_, p)| p.get() == dozer_entity).unwrap_or(false);

        if !is_dozer_e1 && !is_dozer_e2 {
            continue;
        }

        let is_blade_involved = blade_query.contains(e1) || blade_query.contains(e2);
        let cell_candidate = if is_dozer_e1 { e2 } else { e1 };

        let Ok((cell_entity, cell, cell_tf, cell_rb)) = cell_query.get_mut(cell_candidate) else {
            continue;
        };

        // Only process intact static structural cells
        if *cell_rb != RigidBody::Static {
            continue;
        }

        // Contact normal and impulse calculation
        let mut contact_normal = Vec3::Y;
        if let Some(contacts) = collisions.get(e1, e2) {
            if let Some(first_manifold) = contacts.manifolds.first() {
                contact_normal = if is_dozer_e1 {
                    -first_manifold.normal1
                } else {
                    first_manifold.normal1
                };
            }
        }

        // Relative impact velocity along impact direction
        let impact_dir = (cell_tf.translation - dozer_tf.translation).normalize_or_zero();
        let normal_speed = dozer_lin_vel.0.dot(impact_dir).max(0.15);

        // Effective contact mass:
        // Blade has focused hydraulic cutting force multiplier (1.4x), chassis has massive blunt inertia
        let effective_mass = if is_blade_involved {
            4_200.0 // Focused cutting edge contact mass (kg)
        } else {
            3_200.0 // Track/chassis blunt contact mass (kg)
        };

        // Impulse: J = m * delta_v (N·s)
        let impulse = effective_mass * normal_speed;

        info!(
            "Impact with {}: speed={:.2} m/s, impulse={:.1} N·s, threshold={:.1} N·s",
            cell.material_name, normal_speed, impulse, cell.max_impulse_resistance
        );

        if impulse >= cell.max_impulse_resistance {
            // --- FRACTURE NODE ---
            info!(
                "FRACTURED! Structural cell at {:?} shattered by {:.1} N·s impulse!",
                cell.grid_coord, impulse
            );

            // Compute dynamic debris mass
            let volume = 1.44; // 1.2 x 1.0 x 1.2
            let debris_mass = (volume * cell.density).max(80.0);

            // Outward scatter velocity: combine forward momentum with contact normal
            let scatter_dir = (impact_dir * 0.7 + contact_normal * 0.3 + Vec3::Y * 0.4).normalize();
            let scatter_speed = (normal_speed * 1.4 + 1.2).clamp(1.5, 9.0);
            let debris_lin_vel = scatter_dir * scatter_speed;

            // Random rotational tumble
            let tumble_x = ((cell.grid_coord.x as f32 * 17.0).sin() * 4.0).clamp(-4.0, 4.0);
            let tumble_y = ((cell.grid_coord.z as f32 * 23.0).cos() * 5.0).clamp(-5.0, 5.0);
            let tumble_z = ((cell.grid_coord.y as f32 * 31.0).sin() * 4.0).clamp(-4.0, 4.0);
            let debris_ang_vel = Vec3::new(tumble_x, tumble_y, tumble_z);

            // Convert to dynamic physics chunk
            commands
                .entity(cell_entity)
                .remove::<RigidBody>()
                .remove::<StructuralCell>() // Disconnect from structural graph
                .insert((
                    RigidBody::Dynamic,
                    LinearVelocity(debris_lin_vel),
                    AngularVelocity(debris_ang_vel),
                    Mass(debris_mass),
                    LinearDamping(0.25),
                    AngularDamping(0.35),
                    RubbleState::default(),
                ));

            // Momentum transfer: slightly reduce dozer velocity
            let momentum_retention = 1.0 - (debris_mass / dozer_mass.0).clamp(0.05, 0.35);
            dozer_lin_vel.0 *= momentum_retention;

            // Camera shake proportional to fracture impulse
            let trauma = (impulse / (cell.max_impulse_resistance * 2.0))
                .clamp(0.25, 0.85);
            camera_shake.add_trauma(trauma);
        } else {
            // --- YIELD THRESHOLD NOT REACHED ---
            // Kinetic energy absorbed, reducing dozer velocity without fracturing the element
            info!(
                "DEFLECTED! Wall absorbed blow: {:.1} < {:.1} N·s threshold. Dozer slowed.",
                impulse, cell.max_impulse_resistance
            );

            // Heavy bounce / velocity reduction (hitting immovable bedrock or reinforced pillar)
            let bounce_damping = if cell.material_name.contains("pillar") {
                0.15 // Reinforced pillar stops almost dead
            } else {
                0.40 // Brick absorbs heavy momentum
            };
            dozer_lin_vel.0 *= bounce_damping;

            // Heavy thud shake
            camera_shake.add_trauma(0.20);
        }
    }
}
