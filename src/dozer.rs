use avian3d::prelude::*;
use bevy::prelude::*;
use leafwing_input_manager::prelude::*;

use crate::input::PlayerAction;

// --- Vehicle Components (per GDD section 4.2) ---

#[derive(Component)]
#[allow(dead_code)]
pub struct ArmoredDozer {
    pub engine_power: f32,
    pub max_speed: f32,
    pub steer_torque: f32,
    pub left_throttle: f32,  // -1.0 to 1.0
    pub right_throttle: f32, // -1.0 to 1.0
    pub track_width: f32,
}

impl Default for ArmoredDozer {
    fn default() -> Self {
        Self {
            // Komatsu D355A derivative (~65 metric tons fully outfitted)
            // 240,000 N provides ~1.5s acceleration to ~4.2 m/s top speed
            engine_power: 240_000.0,
            max_speed: 4.5,
            steer_torque: 280_000.0,
            left_throttle: 0.0,
            right_throttle: 0.0,
            track_width: 3.2,
        }
    }
}

#[derive(Component)]
pub struct HydraulicBlade {
    pub min_elevation: f32,
    pub max_elevation: f32,
    pub current_elevation: f32,
    pub target_elevation: f32,
    pub lift_speed: f32,
    pub pitch_angle_rad: f32, // 15 degrees forward pitch
}

impl Default for HydraulicBlade {
    fn default() -> Self {
        Self {
            min_elevation: 0.0,
            max_elevation: 1.8,
            current_elevation: 0.0,
            target_elevation: 0.0,
            lift_speed: 1.4, // m/s
            pitch_angle_rad: 15.0_f32.to_radians(),
        }
    }
}

#[derive(Component)]
pub struct BladeColliderMarker;

#[derive(Component)]
#[allow(dead_code)]
pub struct DozerTrackVisual {
    pub is_left: bool,
    pub scroll_offset: f32,
}

// --- PreUpdate: input_action_system ---

pub fn input_action_system(
    action_query: Query<&ActionState<PlayerAction>>,
    mut dozer_query: Query<(&mut ArmoredDozer, &mut HydraulicBlade)>,
) {
    let Ok(action_state) = action_query.get_single() else {
        return;
    };

    let Ok((mut dozer, mut blade)) = dozer_query.get_single_mut() else {
        return;
    };

    // 1. Dual-track Skid Steer Input Handling
    // Check if dedicated dual-track buttons are pressed (Q/Z for left, E/C for right)
    let left_dedicated = if action_state.pressed(&PlayerAction::LeftTrackForward) {
        1.0
    } else if action_state.pressed(&PlayerAction::LeftTrackBackward) {
        -1.0
    } else {
        0.0
    };

    let right_dedicated = if action_state.pressed(&PlayerAction::RightTrackForward) {
        1.0
    } else if action_state.pressed(&PlayerAction::RightTrackBackward) {
        -1.0
    } else {
        0.0
    };

    if left_dedicated != 0.0 || right_dedicated != 0.0 {
        dozer.left_throttle = left_dedicated;
        dozer.right_throttle = right_dedicated;
    } else {
        // Standard Tank Drive (WASD):
        let mut forward_input = 0.0;
        if action_state.pressed(&PlayerAction::DriveForward) {
            forward_input += 1.0;
        }
        if action_state.pressed(&PlayerAction::DriveBackward) {
            forward_input -= 1.0;
        }

        let mut turn_input = 0.0;
        if action_state.pressed(&PlayerAction::TurnLeft) {
            turn_input -= 1.0;
        }
        if action_state.pressed(&PlayerAction::TurnRight) {
            turn_input += 1.0;
        }

        if forward_input != 0.0 || turn_input != 0.0 {
            // When turning while stationary (turn_input != 0, forward_input == 0):
            // Left = +turn, Right = -turn -> pivots directly in place!
            let left: f32 = forward_input + turn_input;
            let right: f32 = forward_input - turn_input;
            dozer.left_throttle = left.clamp(-1.0, 1.0);
            dozer.right_throttle = right.clamp(-1.0, 1.0);
        } else {
            // Smoothly release throttle
            dozer.left_throttle = 0.0;
            dozer.right_throttle = 0.0;
        }
    }

    // 2. Hydraulic Blade Controls
    if action_state.pressed(&PlayerAction::RaiseBlade) {
        blade.target_elevation = blade.max_elevation;
    } else if action_state.pressed(&PlayerAction::LowerBlade) {
        blade.target_elevation = blade.min_elevation;
    }
}

// --- Update: skid_steer_propulsion_system ---

pub fn skid_steer_propulsion_system(
    time: Res<Time>,
    mut dozer_query: Query<(
        &ArmoredDozer,
        &Transform,
        &mut LinearVelocity,
        &mut AngularVelocity,
        &mut ExternalForce,
        &mut ExternalTorque,
    )>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }

    for (dozer, transform, mut lin_vel, ang_vel, mut ext_force, mut ext_torque) in dozer_query.iter_mut() {
        let forward = transform.forward().as_vec3(); // In Bevy, -Z is forward
        let right = transform.right().as_vec3();
        let up = Vec3::Y;

        // Anisotropic lateral friction simulation:
        // Huge resistance against sideways slipping (tracks bite into ground)
        let lateral_speed = lin_vel.0.dot(right);
        let lateral_correction = -right * lateral_speed * (1.0 - (-18.0 * dt).exp());
        lin_vel.0 += lateral_correction;

        // Current longitudinal speed along forward direction
        let current_speed = lin_vel.0.dot(forward);

        // Drive force from left and right tracks
        let avg_throttle = (dozer.left_throttle + dozer.right_throttle) * 0.5;
        let mut forward_force = forward * (avg_throttle * dozer.engine_power);

        // Top speed governor: ease off force when approaching max speed
        if (current_speed > dozer.max_speed && avg_throttle > 0.0)
            || (current_speed < -dozer.max_speed * 0.6 && avg_throttle < 0.0)
        {
            forward_force = Vec3::ZERO;
            // Rolling resistance damping at top speed
            let rolling_drag = -forward * current_speed * 12_000.0 * dt;
            lin_vel.0 += rolling_drag / 60_000.0;
        }

        // Apply longitudinal rolling resistance when throttle is released
        if avg_throttle == 0.0 && current_speed.abs() > 0.05 {
            let rolling_resistance = -forward * current_speed.signum() * 18_000.0 * dt;
            let dv = rolling_resistance / 60_000.0;
            if dv.length() > current_speed.abs() {
                lin_vel.0 -= forward * current_speed;
            } else {
                lin_vel.0 += dv;
            }
        }

        // Steering torque from differential track speeds:
        // Left track forward (+1) and right track backward (-1) creates clockwise rotation
        let diff_throttle = dozer.left_throttle - dozer.right_throttle;
        let yaw_torque = -up * (diff_throttle * dozer.steer_torque);

        // Yaw damping to prevent uncontrolled spinning
        let current_yaw_rate = ang_vel.0.y;
        let yaw_damping = -up * (current_yaw_rate * 80_000.0);

        // Keep bulldozer upright (pitch/roll stability against tipping over)
        let tilt_x = transform.rotation.x;
        let tilt_z = transform.rotation.z;
        let upright_torque = Vec3::new(-tilt_x * 400_000.0, 0.0, -tilt_z * 400_000.0);

        ext_force.clear();
        ext_force.apply_force(forward_force);
        ext_torque.clear();
        ext_torque.apply_torque(yaw_torque + yaw_damping + upright_torque);
    }
}

// --- Update: blade_actuation_system ---

pub fn blade_actuation_system(
    time: Res<Time>,
    mut blade_query: Query<(&mut HydraulicBlade, &Children)>,
    mut child_transform_query: Query<
        &mut Transform,
        (With<BladeColliderMarker>, Without<ArmoredDozer>),
    >,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }

    for (mut blade, children) in blade_query.iter_mut() {
        // Smoothly interpolate current elevation towards target elevation
        let diff = blade.target_elevation - blade.current_elevation;
        let step = blade.lift_speed * dt;

        if diff.abs() <= step {
            blade.current_elevation = blade.target_elevation;
        } else {
            blade.current_elevation += diff.signum() * step;
        }

        // Base local position of the blade: in front of dozer (-Z in Bevy coords), at elevation height
        let base_z = -3.2; // 3.2 meters forward from center
        let base_y = 0.5 + blade.current_elevation;

        for &child in children.iter() {
            if let Ok(mut transform) = child_transform_query.get_mut(child) {
                transform.translation = Vec3::new(0.0, base_y, base_z);
                // 15 degrees forward pitch
                transform.rotation = Quat::from_rotation_x(blade.pitch_angle_rad);
            }
        }
    }
}

// --- Spawner Helper for Armored Dozer Assembly ---

pub fn spawn_armored_dozer(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
    spawn_pos: Vec3,
) -> Entity {
    // Industrial & Armor Materials
    let armor_plating_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.22, 0.24, 0.26), // Heavy dark welded composite steel
        metallic: 0.85,
        perceptual_roughness: 0.45,
        ..default()
    });

    let industrial_yellow_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.92, 0.68, 0.08), // Komatsu construction yellow
        metallic: 0.25,
        perceptual_roughness: 0.5,
        ..default()
    });

    let track_steel_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.12, 0.12, 0.13), // Cast manganese steel treads
        metallic: 0.95,
        perceptual_roughness: 0.6,
        ..default()
    });

    let reinforced_blade_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.35, 0.38, 0.42), // Hardox 500 reinforced wear plate
        metallic: 0.9,
        perceptual_roughness: 0.35,
        ..default()
    });

    let cabin_slit_glass_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.1, 0.2, 0.25),
        metallic: 0.1,
        perceptual_roughness: 0.1,
        ..default()
    });

    // Mesh primitives
    let chassis_mesh = meshes.add(Cuboid::new(3.0, 1.8, 5.2));
    let armor_hood_mesh = meshes.add(Cuboid::new(2.4, 0.9, 3.0));
    let armored_cab_mesh = meshes.add(Cuboid::new(2.2, 1.3, 1.8));
    let view_slit_mesh = meshes.add(Cuboid::new(1.6, 0.18, 0.1));
    let track_mesh = meshes.add(Cuboid::new(0.7, 1.1, 5.6));
    let blade_mesh = meshes.add(Cuboid::new(4.2, 1.4, 0.45));
    let blade_push_arm_mesh = meshes.add(Cuboid::new(0.25, 0.35, 2.8));

    // Spawn Main Chassis Root (RigidBody Dynamic with 60,000 kg mass)
    let dozer_entity = commands
        .spawn((
            ArmoredDozer::default(),
            HydraulicBlade::default(),
            RigidBody::Dynamic,
            // Chassis physical collider: 3.0m wide, 1.8m high, 5.2m long
            Collider::cuboid(3.0, 1.8, 5.2),
            Mass(60_000.0),
            // High rotational inertia along Y-axis to prevent instant spinning
            AngularInertia::new(Vec3::new(120_000.0, 350_000.0, 120_000.0)),
            LinearDamping(0.3),
            AngularDamping(2.2),
            ExternalForce::default(),
            ExternalTorque::default(),
            Transform::from_translation(spawn_pos),
            crate::input::default_input_map(),
            ActionState::<PlayerAction>::default(),
            Visibility::default(),
        ))
        .with_children(|parent| {
            // Main Chassis Visual
            parent.spawn((
                Mesh3d(chassis_mesh),
                MeshMaterial3d(industrial_yellow_mat.clone()),
                Transform::from_xyz(0.0, 0.9, 0.0),
            ));

            // Armored Sloped Engine Hood
            parent.spawn((
                Mesh3d(armor_hood_mesh),
                MeshMaterial3d(armor_plating_mat.clone()),
                Transform::from_xyz(0.0, 1.8, -0.9),
            ));

            // Armored Cockpit Cab
            parent.spawn((
                Mesh3d(armored_cab_mesh),
                MeshMaterial3d(armor_plating_mat.clone()),
                Transform::from_xyz(0.0, 2.2, 1.1),
            ));

            // Armored Viewport Slit (Bulletproof narrow vision block)
            parent.spawn((
                Mesh3d(view_slit_mesh),
                MeshMaterial3d(cabin_slit_glass_mat),
                Transform::from_xyz(0.0, 2.2, 0.18),
            ));

            // Left Continuous Track Assembly
            parent.spawn((
                Mesh3d(track_mesh.clone()),
                MeshMaterial3d(track_steel_mat.clone()),
                Transform::from_xyz(-1.7, 0.55, 0.0),
                DozerTrackVisual {
                    is_left: true,
                    scroll_offset: 0.0,
                },
            ));

            // Right Continuous Track Assembly
            parent.spawn((
                Mesh3d(track_mesh),
                MeshMaterial3d(track_steel_mat),
                Transform::from_xyz(1.7, 0.55, 0.0),
                DozerTrackVisual {
                    is_left: false,
                    scroll_offset: 0.0,
                },
            ));

            // Left Hydraulic Push Arm
            parent.spawn((
                Mesh3d(blade_push_arm_mesh.clone()),
                MeshMaterial3d(armor_plating_mat.clone()),
                Transform::from_xyz(-1.8, 0.6, -1.8),
            ));

            // Right Hydraulic Push Arm
            parent.spawn((
                Mesh3d(blade_push_arm_mesh),
                MeshMaterial3d(armor_plating_mat),
                Transform::from_xyz(1.8, 0.6, -1.8),
            ));

            // Hydraulic Armored Blade Assembly (5,000 kg assembly)
            parent.spawn((
                BladeColliderMarker,
                // Blade physical collider: 4.2m wide, 1.4m high, 0.45m thick
                Collider::cuboid(4.2, 1.4, 0.45),
                Mesh3d(blade_mesh),
                MeshMaterial3d(reinforced_blade_mat),
                Transform::from_xyz(0.0, 0.7, -3.2)
                    .with_rotation(Quat::from_rotation_x(15.0_f32.to_radians())),
            ));
        })
        .id();

    dozer_entity
}
