mod building;
mod camera;
mod collision;
mod debris;
mod dozer;
mod input;
mod ui;

use avian3d::prelude::*;
use bevy::prelude::*;
use leafwing_input_manager::prelude::*;

use building::{
    reload_building_system, spawn_building_from_spec, structural_integrity_bfs_system,
    BfsTimer, BuildingSpec, BuildingStats, ReloadBuildingEvent,
};
use camera::{isometric_camera_follow_system, CameraShake, IsometricCamera};
use collision::collision_impulse_check_system;
use debris::{debris_lifecycle_system, DebrisStats};
use dozer::{
    blade_actuation_system, input_action_system, skid_steer_propulsion_system, spawn_armored_dozer,
};
use input::PlayerAction;
use ui::{setup_hud, update_hud_system};

fn main() {
    App::new()
        // Window & Engine configuration
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Permit Denied // Milestone 1: Mechanical Sandbox".into(),
                resolution: (1280.0_f32, 720.0_f32).into(),
                present_mode: bevy::window::PresentMode::AutoVsync,
                fit_canvas_to_parent: true,
                ..default()
            }),
            ..default()
        }))
        // Avian 3D Physics Engine
        .add_plugins(PhysicsPlugins::default())
        // Leafwing Input Manager
        .add_plugins(InputManagerPlugin::<PlayerAction>::default())
        // Resources & Events
        .init_resource::<CameraShake>()
        .init_resource::<BfsTimer>()
        .init_resource::<BuildingStats>()
        .init_resource::<DebrisStats>()
        .add_event::<ReloadBuildingEvent>()
        // Startup Systems
        .add_systems(Startup, (setup_world, setup_hud))
        // PreUpdate
        .add_systems(PreUpdate, (input_action_system, handle_reload_input))
        // Update (60 Hz locomotion, actuation, collision impulses)
        .add_systems(
            Update,
            (
                skid_steer_propulsion_system,
                blade_actuation_system,
                collision_impulse_check_system,
                reload_building_system,
                update_hud_system,
            ),
        )
        // FixedUpdate (10 Hz BFS structural reachability cycle)
        .add_systems(FixedUpdate, structural_integrity_bfs_system)
        // PostUpdate (Settling, camera follow & screen shake)
        .add_systems(
            PostUpdate,
            (debris_lifecycle_system, isometric_camera_follow_system),
        )
        .run();
}

fn setup_world(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // 1. Ambient & Directional Lighting
    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.9, 0.92, 0.95),
        brightness: 450.0,
    });

    commands.spawn((
        DirectionalLight {
            illuminance: 12_000.0,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(30.0, 45.0, 25.0).looking_at(Vec3::new(0.0, 0.0, -5.0), Vec3::Y),
    ));

    // 2. Ground Terrain (Asphalt / Demolition Yard)
    let ground_size = 140.0;
    let ground_thickness = 1.0;
    let ground_mesh = meshes.add(Plane3d::default().mesh().size(ground_size, ground_size));
    let ground_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.18, 0.19, 0.20),
        metallic: 0.1,
        perceptual_roughness: 0.9,
        ..default()
    });

    // Spawn physical ground collider at Y = -0.5 so top surface is at Y = 0.0
    commands.spawn((
        RigidBody::Static,
        Collider::cuboid(ground_size, ground_thickness, ground_size),
        Friction::new(0.2),
        Mesh3d(ground_mesh),
        MeshMaterial3d(ground_mat),
        Transform::from_xyz(0.0, -0.5, 0.0),
    ));

    // Ground boundary berms (prevent falling off world)
    let berm_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.14, 0.15, 0.16),
        metallic: 0.1,
        perceptual_roughness: 0.95,
        ..default()
    });
    let berm_h = 3.0;
    let berm_mesh_x = meshes.add(Cuboid::new(ground_size, berm_h, 2.0));
    let berm_mesh_z = meshes.add(Cuboid::new(2.0, berm_h, ground_size));

    let half = ground_size * 0.5;
    // North Berm
    commands.spawn((
        RigidBody::Static,
        Collider::cuboid(ground_size, berm_h, 2.0),
        Mesh3d(berm_mesh_x.clone()),
        MeshMaterial3d(berm_mat.clone()),
        Transform::from_xyz(0.0, berm_h * 0.5, -half),
    ));
    // South Berm
    commands.spawn((
        RigidBody::Static,
        Collider::cuboid(ground_size, berm_h, 2.0),
        Mesh3d(berm_mesh_x),
        MeshMaterial3d(berm_mat.clone()),
        Transform::from_xyz(0.0, berm_h * 0.5, half),
    ));
    // West Berm
    commands.spawn((
        RigidBody::Static,
        Collider::cuboid(2.0, berm_h, ground_size),
        Mesh3d(berm_mesh_z.clone()),
        MeshMaterial3d(berm_mat.clone()),
        Transform::from_xyz(-half, berm_h * 0.5, 0.0),
    ));
    // East Berm
    commands.spawn((
        RigidBody::Static,
        Collider::cuboid(2.0, berm_h, ground_size),
        Mesh3d(berm_mesh_z),
        MeshMaterial3d(berm_mat),
        Transform::from_xyz(half, berm_h * 0.5, 0.0),
    ));

    // 3. Spawn Armored Dozer
    let dozer_spawn_pos = Vec3::new(0.0, 1.05, 12.0);
    spawn_armored_dozer(&mut commands, &mut meshes, &mut materials, dozer_spawn_pos);

    // 4. Spawn Data-Driven Destructible Building from RON
    let ron_path = "assets/buildings/sample_hardware_store.building.ron";
    if let Ok(content) = std::fs::read_to_string(ron_path) {
        match ron::from_str::<BuildingSpec>(&content) {
            Ok(spec) => {
                let building_origin = Vec3::new(0.0, 0.0, -10.0);
                spawn_building_from_spec(
                    &mut commands,
                    &spec,
                    building_origin,
                    &mut meshes,
                    &mut materials,
                );
                info!("Loaded initial building: {}", spec.name);
            }
            Err(e) => {
                error!("Error parsing initial building RON: {:?}", e);
            }
        }
    } else {
        error!("Could not read initial building RON at: {}", ron_path);
    }

    // 5. Spawn Isometric Camera with Screen Shake
    commands.spawn((
        Camera3d::default(),
        IsometricCamera {
            target_offset: Vec3::new(-24.0, 22.0, 24.0),
            follow_speed: 5.0,
            current_target: dozer_spawn_pos,
        },
        Transform::from_xyz(-24.0, 23.05, 36.0).looking_at(dozer_spawn_pos, Vec3::Y),
    ));
}

fn handle_reload_input(
    keyboard: Res<ButtonInput<KeyCode>>,
    action_query: Query<&ActionState<PlayerAction>>,
    mut reload_events: EventWriter<ReloadBuildingEvent>,
) {
    let triggered = keyboard.just_pressed(KeyCode::KeyB)
        || action_query
            .get_single()
            .map(|a| a.just_pressed(&PlayerAction::ResetBuilding))
            .unwrap_or(false);

    if triggered {
        info!("ResetBuilding action triggered by player [B]");
        reload_events.send_default();
    }
}
