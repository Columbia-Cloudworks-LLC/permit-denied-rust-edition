//! Debug overlays: Avian2D collider outlines projected into the isometric view (F4).
//!
//! Avian's own `PhysicsDebugPlugin` draws in Cartesian physics space, which does not line up
//! with the isometric render. This overlay walks each collider's shape (cuboids, balls and
//! compounds of them; anything else falls back to its AABB) and draws it through
//! `world_to_iso_screen`, so outlines sit on the sprites they belong to.

use avian2d::parry::shape::SharedShape;
use avian2d::prelude::*;
use bevy::prelude::*;
use pd_core::world_to_iso_screen;

/// Whether the iso collider overlay is drawn.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColliderOverlay(pub bool);

const STATIC_COLOR: Color = Color::srgb(0.2, 0.9, 1.0);
const DYNAMIC_COLOR: Color = Color::srgb(1.0, 0.55, 0.1);

fn draw_polyline(gizmos: &mut Gizmos, world_points: &[Vec2], color: Color) {
    let Some(&first) = world_points.first() else {
        return;
    };
    let screen = world_points
        .iter()
        .chain(std::iter::once(&first))
        .map(|p| world_to_iso_screen(*p, 0.0));
    gizmos.linestrip_2d(screen, color);
}

fn draw_leaf(gizmos: &mut Gizmos, shape: &SharedShape, origin: Vec2, angle: f32, color: Color) {
    let rot = Vec2::from_angle(angle);
    if let Some(cuboid) = shape.as_cuboid() {
        let h = Vec2::new(cuboid.half_extents.x, cuboid.half_extents.y);
        let corners = [
            Vec2::new(-h.x, -h.y),
            Vec2::new(h.x, -h.y),
            Vec2::new(h.x, h.y),
            Vec2::new(-h.x, h.y),
        ]
        .map(|c| origin + rot.rotate(c));
        draw_polyline(gizmos, &corners, color);
    } else if let Some(ball) = shape.as_ball() {
        let points: Vec<Vec2> = (0..16)
            .map(|i| origin + Vec2::from_angle(i as f32 * std::f32::consts::TAU / 16.0) * ball.radius)
            .collect();
        draw_polyline(gizmos, &points, color);
    } else {
        let aabb = shape.compute_local_aabb();
        let corners = [
            Vec2::new(aabb.mins.x, aabb.mins.y),
            Vec2::new(aabb.maxs.x, aabb.mins.y),
            Vec2::new(aabb.maxs.x, aabb.maxs.y),
            Vec2::new(aabb.mins.x, aabb.maxs.y),
        ]
        .map(|c| origin + rot.rotate(c));
        draw_polyline(gizmos, &corners, color);
    }
}

/// `Update`: draws every collider's outline in isometric screen space when enabled.
pub fn draw_iso_collider_overlay_system(
    overlay: Res<ColliderOverlay>,
    mut gizmos: Gizmos,
    colliders: Query<(&Collider, &Position, &Rotation, Option<&RigidBody>)>,
) {
    if !overlay.0 {
        return;
    }
    for (collider, position, rotation, body) in &colliders {
        let color = if matches!(body, Some(RigidBody::Dynamic)) {
            DYNAMIC_COLOR
        } else {
            STATIC_COLOR
        };
        let origin = position.0;
        let angle = rotation.as_radians();
        if !origin.is_finite() || !angle.is_finite() {
            continue;
        }
        let shape = collider.shape_scaled();
        if let Some(compound) = shape.as_compound() {
            for (iso, sub) in compound.shapes() {
                let offset = Vec2::new(iso.translation.vector.x, iso.translation.vector.y);
                let sub_origin = origin + Vec2::from_angle(angle).rotate(offset);
                draw_leaf(&mut gizmos, sub, sub_origin, angle + iso.rotation.angle(), color);
            }
        } else {
            draw_leaf(&mut gizmos, shape, origin, angle, color);
        }
    }
}

/// Registers the overlay resource and draw system.
pub struct DebugOverlaysPlugin;

impl Plugin for DebugOverlaysPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ColliderOverlay>()
            .add_systems(Update, draw_iso_collider_overlay_system);
    }
}
