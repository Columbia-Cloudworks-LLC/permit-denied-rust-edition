use bevy::prelude::*;

#[derive(Component)]
pub struct IsometricCamera {
    pub target_offset: Vec3,
    pub follow_speed: f32,
    pub current_target: Vec3,
}

impl Default for IsometricCamera {
    fn default() -> Self {
        Self {
            // Isometric perspective offset: 45 degree angle in XZ plane, tilted up
            target_offset: Vec3::new(-24.0, 22.0, 24.0),
            follow_speed: 4.5,
            current_target: Vec3::ZERO,
        }
    }
}

#[derive(Resource)]
pub struct CameraShake {
    pub trauma: f32,
    pub max_angle: f32,  // in radians
    pub max_offset: f32, // in meters
    pub decay_rate: f32,
    pub time_elapsed: f32,
}

impl Default for CameraShake {
    fn default() -> Self {
        Self {
            trauma: 0.0,
            max_angle: 0.05,  // ~2.8 degrees
            max_offset: 0.35, // 35 cm offset
            decay_rate: 1.6,
            time_elapsed: 0.0,
        }
    }
}

impl CameraShake {
    pub fn add_trauma(&mut self, amount: f32) {
        self.trauma = (self.trauma + amount).clamp(0.0, 1.0);
    }
}

pub fn isometric_camera_follow_system(
    time: Res<Time>,
    mut shake: ResMut<CameraShake>,
    target_query: Query<&Transform, (With<crate::dozer::ArmoredDozer>, Without<IsometricCamera>)>,
    mut camera_query: Query<(&mut Transform, &mut IsometricCamera)>,
) {
    let dt = time.delta_secs();
    shake.time_elapsed += dt;

    // Decay trauma over time
    if shake.trauma > 0.0 {
        shake.trauma = (shake.trauma - shake.decay_rate * dt).max(0.0);
    }

    let Ok((mut cam_transform, mut iso_cam)) = camera_query.get_single_mut() else {
        return;
    };

    let target_pos = if let Ok(target_tf) = target_query.get_single() {
        target_tf.translation
    } else {
        Vec3::ZERO
    };

    // Smooth follow damping
    iso_cam.current_target = iso_cam
        .current_target
        .lerp(target_pos, (iso_cam.follow_speed * dt).min(1.0));

    let base_cam_pos = iso_cam.current_target + iso_cam.target_offset;

    // Non-linear trauma response: shake = trauma^2
    let shake_intensity = shake.trauma * shake.trauma;

    let t = shake.time_elapsed * 32.0; // High-frequency vibration
    let shake_offset = if shake_intensity > 0.001 {
        Vec3::new(
            (t * 1.1).sin() * (t * 0.7).cos() * shake.max_offset * shake_intensity,
            (t * 1.3).cos() * (t * 0.9).sin() * (shake.max_offset * 0.6) * shake_intensity,
            (t * 1.7).sin() * (t * 1.2).sin() * shake.max_offset * shake_intensity,
        )
    } else {
        Vec3::ZERO
    };

    cam_transform.translation = base_cam_pos + shake_offset;

    // Look at target with shake rotational wobble
    let look_target = iso_cam.current_target
        + if shake_intensity > 0.001 {
            Vec3::new(
                (t * 1.5).cos() * shake.max_offset * 0.5 * shake_intensity,
                0.0,
                (t * 1.4).sin() * shake.max_offset * 0.5 * shake_intensity,
            )
        } else {
            Vec3::ZERO
        };

    let look_dir = (look_target - cam_transform.translation).normalize_or_zero();
    if look_dir.length_squared() > 0.001 {
        cam_transform.look_at(look_target, Vec3::Y);

        // Add subtle roll shake
        if shake_intensity > 0.001 {
            let roll = (t * 2.1).sin() * shake.max_angle * shake_intensity;
            cam_transform.rotation *= Quat::from_rotation_z(roll);
        }
    }
}
