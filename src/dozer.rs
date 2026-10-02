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
    keyboard: Res<ButtonInput<KeyCode>>,
    action_query: Query<&ActionState<PlayerAction>>,
    mut dozer_query: Query<(&mut ArmoredDozer, &mut HydraulicBlade)>,
) {
    let Ok((mut dozer, mut blade)) = dozer_query.get_single_mut() else {
        return;
    };

    let mut forward_input = 0.0f32;
    let mut turn_input = 0.0f32;

    // 1. Direct Keyboard Detection (WASD / Arrows)
    if keyboard.pressed(KeyCode::KeyW) || keyboard.pressed(KeyCode::ArrowUp) {
        forward_input += 1.0;
    }
    if keyboard.pressed(KeyCode::KeyS) || keyboard.pressed(KeyCode::ArrowDown) {
        forward_input -= 1.0;
    }
    if keyboard.pressed(KeyCode::KeyA) || keyboard.pressed(KeyCode::ArrowLeft) {
        turn_input -= 1.0;
    }
    if keyboard.pressed(KeyCode::KeyD) || keyboard.pressed(KeyCode::ArrowRight) {
        turn_input += 1.0;
    }

    // Direct Dual-Track control (Q/Z left track, E/C right track)
    let mut left_direct = 0.0f32;
    let mut right_direct = 0.0f32;
    if keyboard.pressed(KeyCode::KeyQ) {
        left_direct += 1.0;
    }
    if keyboard.pressed(KeyCode::KeyZ) {
        left_direct -= 1.0;
    }
    if keyboard.pressed(KeyCode::KeyE) {
        right_direct += 1.0;
    }
    if keyboard.pressed(KeyCode::KeyC) {
        right_direct -= 1.0;
    }

    // 2. Leafwing Action State Integration (Gamepad / Rebound keys)
    if let Ok(action_state) = action_query.get_single() {
        if action_state.pressed(&PlayerAction::DriveForward) {
            forward_input = forward_input.max(1.0);
        }
        if action_state.pressed(&PlayerAction::DriveBackward) {
            forward_input = forward_input.min(-1.0);
        }
        if action_state.pressed(&PlayerAction::TurnLeft) {
            turn_input = turn_input.min(-1.0);
        }
        if action_state.pressed(&PlayerAction::TurnRight) {
            turn_input = turn_input.max(1.0);
        }

        if action_state.pressed(&PlayerAction::LeftTrackForward) {
            left_direct = left_direct.max(1.0);
        }
        if action_state.pressed(&PlayerAction::LeftTrackBackward) {
            left_direct = left_direct.min(-1.0);
        }
        if action_state.pressed(&PlayerAction::RightTrackForward) {
            right_direct = right_direct.max(1.0);
        }
        if action_state.pressed(&PlayerAction::RightTrackBackward) {
            right_direct = right_direct.min(-1.0);
        }
    }

    // 3. Resolve Track Throttles
    if left_direct != 0.0 || right_direct != 0.0 {
        dozer.left_throttle = left_direct;
        dozer.right_throttle = right_direct;
    } else if forward_input != 0.0 || turn_input != 0.0 {
        // Tank drive mixing:
        // Turning stationary (turn != 0, forward == 0): Left = +turn, Right = -turn -> pivots directly in place!
        let left: f32 = forward_input + turn_input;
        let right: f32 = forward_input - turn_input;
        dozer.left_throttle = left.clamp(-1.0, 1.0);
        dozer.right_throttle = right.clamp(-1.0, 1.0);
    } else {
        dozer.left_throttle = 0.0;
        dozer.right_throttle = 0.0;
    }

    // 4. Hydraulic Blade Controls (Space / R = Raise, Shift / F = Lower)
    let raise = keyboard.pressed(KeyCode::Space)
        || keyboard.pressed(KeyCode::KeyR)
        || action_query
            .get_single()
            .map(|a| a.pressed(&PlayerAction::RaiseBlade))
            .unwrap_or(false);

    let lower = keyboard.pressed(KeyCode::ShiftLeft)
        || keyboard.pressed(KeyCode::ShiftRight)
        || keyboard.pressed(KeyCode::KeyF)
        || action_query
            .get_single()
            .map(|a| a.pressed(&PlayerAction::LowerBlade))
            .unwrap_or(false);

    if raise {
        blade.target_elevation = blade.max_elevation;
    } else if lower {
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
    )>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }

    for (dozer, transform, mut lin_vel, mut ang_vel) in dozer_query.iter_mut() {
        let forward = transform.forward().as_vec3(); // In Bevy, -Z is forward
        let right = transform.right().as_vec3();

        // 1. Anisotropic lateral friction simulation:
        // Scrub lateral velocity completely so tracks bite into ground without sliding sideways
        let lateral_speed = lin_vel.0.dot(right);
        lin_vel.0 -= right * lateral_speed;

        // 2. Track Propulsion Mechanics
        let avg_throttle = (dozer.left_throttle + dozer.right_throttle) * 0.5;
        let diff_throttle = dozer.left_throttle - dozer.right_throttle;
        let current_speed = lin_vel.0.dot(forward);

        if avg_throttle.abs() > 0.01 {
            // Acceleration rate: ~1.5s to reach top speed of 4.5 m/s (a = 3.0 m/s^2)
            let accel = 3.2;
            let target_speed = avg_throttle * dozer.max_speed;
            let speed_diff = target_speed - current_speed;
            let step = (accel * dt).min(speed_diff.abs());
            lin_vel.0 += forward * (speed_diff.signum() * step);
        } else {
            // Natural rolling drag when throttle released (low rolling resistance, heavy momentum carry)
            if current_speed.abs() > 0.05 {
                let drag = 0.75 * dt;
                if drag > current_speed.abs() {
                    lin_vel.0 -= forward * current_speed;
                } else {
                    lin_vel.0 -= forward * (current_speed.signum() * drag);
                }
            } else {
                lin_vel.0 -= forward * current_speed;
            }
        }

        // 3. Skid-Steering Differential Rotation (In-place pivoting)
        if diff_throttle.abs() > 0.01 {
            let target_yaw_rate = -diff_throttle * 1.1; // ~1.1 rad/s pivot speed
            let yaw_diff = target_yaw_rate - ang_vel.0.y;
            let step = (3.5 * dt).min(yaw_diff.abs());
            ang_vel.0.y += yaw_diff.signum() * step;
        } else {
            // Heavy yaw damping stops rotation quickly when steering input is released
            ang_vel.0.y *= (1.0 - 8.0 * dt).max(0.0);
        }

        // 4. Pitch & roll stabilization to keep 60-ton dozer firmly planted upright
        ang_vel.0.x *= (1.0 - 10.0 * dt).max(0.0);
        ang_vel.0.z *= (1.0 - 10.0 * dt).max(0.0);
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

        // Base local position of the blade: in front of dozer (-Z), at elevation height
        let base_z = -3.2; // 3.2 meters forward from center
        let base_y = 0.7 + blade.current_elevation;

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
            // Low belly friction so tracks drive smoothly over terrain
            Friction::new(0.05),
            AngularInertia::new(Vec3::new(120_000.0, 350_000.0, 120_000.0)),
            LinearDamping(0.2),
            AngularDamping(2.0),
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
                Transform::from_xyz(0.0, 0.0, 0.0),
            ));

            // Armored Sloped Engine Hood
            parent.spawn((
                Mesh3d(armor_hood_mesh),
                MeshMaterial3d(armor_plating_mat.clone()),
                Transform::from_xyz(0.0, 0.9, -0.9),
            ));

            // Armored Cockpit Cab
            parent.spawn((
                Mesh3d(armored_cab_mesh),
                MeshMaterial3d(armor_plating_mat.clone()),
                Transform::from_xyz(0.0, 1.3, 1.1),
            ));

            // Armored Viewport Slit (Bulletproof narrow vision block)
            parent.spawn((
                Mesh3d(view_slit_mesh),
                MeshMaterial3d(cabin_slit_glass_mat),
                Transform::from_xyz(0.0, 1.3, 0.18),
            ));

            // Left Continuous Track Assembly
            parent.spawn((
                Mesh3d(track_mesh.clone()),
                MeshMaterial3d(track_steel_mat.clone()),
                Transform::from_xyz(-1.7, -0.35, 0.0),
                DozerTrackVisual {
                    is_left: true,
                    scroll_offset: 0.0,
                },
            ));

            // Right Continuous Track Assembly
            parent.spawn((
                Mesh3d(track_mesh),
                MeshMaterial3d(track_steel_mat),
                Transform::from_xyz(1.7, -0.35, 0.0),
                DozerTrackVisual {
                    is_left: false,
                    scroll_offset: 0.0,
                },
            ));

            // Left Hydraulic Push Arm
            parent.spawn((
                Mesh3d(blade_push_arm_mesh.clone()),
                MeshMaterial3d(armor_plating_mat.clone()),
                Transform::from_xyz(-1.8, -0.2, -1.8),
            ));

            // Right Hydraulic Push Arm
            parent.spawn((
                Mesh3d(blade_push_arm_mesh),
                MeshMaterial3d(armor_plating_mat),
                Transform::from_xyz(1.8, -0.2, -1.8),
            ));

            // Hydraulic Armored Blade Assembly (5,000 kg assembly)
            parent.spawn((
                BladeColliderMarker,
                // Blade physical collider: 4.2m wide, 1.4m high, 0.45m thick
                Collider::cuboid(4.2, 1.4, 0.45),
                Friction::new(0.1),
                Mesh3d(blade_mesh),
                MeshMaterial3d(reinforced_blade_mat),
                Transform::from_xyz(0.0, 0.7, -3.2)
                    .with_rotation(Quat::from_rotation_x(15.0_f32.to_radians())),
            ));
        })
        .id();

    dozer_entity
}
