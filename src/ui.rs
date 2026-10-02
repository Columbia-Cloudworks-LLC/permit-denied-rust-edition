use avian3d::prelude::*;
use bevy::prelude::*;

use crate::building::BuildingStats;
use crate::debris::DebrisStats;
use crate::dozer::{ArmoredDozer, HydraulicBlade};

#[derive(Component)]
pub struct HudSpeedText;

#[derive(Component)]
pub struct HudBladeText;

#[derive(Component)]
pub struct HudBuildingText;

#[derive(Component)]
pub struct HudPerformanceText;

pub fn setup_hud(mut commands: Commands) {
    // Root UI container
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::all(Val::Px(16.0)),
                ..default()
            },
            // Pickable::IGNORE in Bevy 0.15 for HUD overlays
            PickingBehavior::IGNORE,
        ))
        .with_children(|parent| {
            // Top Bar: Game Title & Demolition Target
            parent
                .spawn(Node {
                    flex_direction: FlexDirection::Row,
                    justify_content: JustifyContent::SpaceBetween,
                    align_items: AlignItems::Center,
                    width: Val::Percent(100.0),
                    ..default()
                })
                .with_children(|top_row| {
                    // Title Box
                    top_row
                        .spawn((
                            Node {
                                padding: UiRect::axes(Val::Px(16.0), Val::Px(8.0)),
                                border: UiRect::all(Val::Px(2.0)),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.08, 0.09, 0.10, 0.88)),
                            BorderColor(Color::srgba(0.92, 0.68, 0.08, 0.9)),
                        ))
                        .with_children(|box_node| {
                            box_node.spawn((
                                Text::new("PERMIT DENIED  //  MILESTONE 1: MECHANICAL SANDBOX"),
                                TextFont {
                                    font_size: 17.0,
                                    ..default()
                                },
                                TextColor(Color::srgb(0.95, 0.72, 0.1)),
                            ));
                        });

                    // Controls Legend Pill
                    top_row
                        .spawn((
                            Node {
                                padding: UiRect::axes(Val::Px(14.0), Val::Px(6.0)),
                                border: UiRect::all(Val::Px(1.0)),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.06, 0.07, 0.08, 0.85)),
                            BorderColor(Color::srgba(0.4, 0.45, 0.5, 0.5)),
                        ))
                        .with_children(|pill| {
                            pill.spawn((
                                Text::new("[W/S] Drive  [A/D] Pivot  [Space/Shift] Blade Up/Down  [B] Rebuild Warehouse"),
                                TextFont {
                                    font_size: 13.0,
                                    ..default()
                                },
                                TextColor(Color::srgb(0.85, 0.88, 0.92)),
                            ));
                        });
                });

            // Bottom Bar: Telemetry Panels
            parent
                .spawn(Node {
                    flex_direction: FlexDirection::Row,
                    justify_content: JustifyContent::SpaceBetween,
                    align_items: AlignItems::FlexEnd,
                    width: Val::Percent(100.0),
                    ..default()
                })
                .with_children(|bottom_row| {
                    // Left Panel: Dozer Locomotion & Blade Hydraulics
                    bottom_row
                        .spawn((
                            Node {
                                flex_direction: FlexDirection::Column,
                                padding: UiRect::all(Val::Px(14.0)),
                                border: UiRect::all(Val::Px(1.5)),
                                width: Val::Px(360.0),
                                row_gap: Val::Px(6.0),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.05, 0.06, 0.07, 0.90)),
                            BorderColor(Color::srgba(0.35, 0.38, 0.42, 0.7)),
                        ))
                        .with_children(|panel| {
                            panel.spawn((
                                Text::new("KOMATSU D355A HEAVY ARMORED DOZER"),
                                TextFont {
                                    font_size: 14.0,
                                    ..default()
                                },
                                TextColor(Color::srgb(0.92, 0.68, 0.08)),
                            ));

                            panel.spawn((
                                Text::new("Speed: 0.0 m/s (0.0 km/h)  |  Momentum: 0 kJ"),
                                TextFont {
                                    font_size: 13.0,
                                    ..default()
                                },
                                TextColor(Color::srgb(0.8, 0.85, 0.9)),
                                HudSpeedText,
                            ));

                            panel.spawn((
                                Text::new("Blade Elevation: 0.00m / 1.80m [15° Attack Angle]"),
                                TextFont {
                                    font_size: 13.0,
                                    ..default()
                                },
                                TextColor(Color::srgb(0.4, 0.85, 0.95)),
                                HudBladeText,
                            ));
                        });

                    // Right Panel: Building Graph & Performance Debris Budget
                    bottom_row
                        .spawn((
                            Node {
                                flex_direction: FlexDirection::Column,
                                padding: UiRect::all(Val::Px(14.0)),
                                border: UiRect::all(Val::Px(1.5)),
                                width: Val::Px(360.0),
                                row_gap: Val::Px(6.0),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.05, 0.06, 0.07, 0.90)),
                            BorderColor(Color::srgba(0.35, 0.38, 0.42, 0.7)),
                        ))
                        .with_children(|panel| {
                            panel.spawn((
                                Text::new("GRANBY HARDWARE WAREHOUSE // GRAPH INTEGRITY"),
                                TextFont {
                                    font_size: 14.0,
                                    ..default()
                                },
                                TextColor(Color::srgb(0.9, 0.4, 0.3)),
                            ));

                            panel.spawn((
                                Text::new("Intact Cells: -- / -- (Roof: 100%)"),
                                TextFont {
                                    font_size: 13.0,
                                    ..default()
                                },
                                TextColor(Color::srgb(0.88, 0.88, 0.88)),
                                HudBuildingText,
                            ));

                            panel.spawn((
                                Text::new("Active Debris: 0 / 400 Max  |  Baked: 0"),
                                TextFont {
                                    font_size: 13.0,
                                    ..default()
                                },
                                TextColor(Color::srgb(0.5, 0.9, 0.5)),
                                HudPerformanceText,
                            ));
                        });
                });
        });
}

pub fn update_hud_system(
    dozer_query: Query<(&LinearVelocity, &ArmoredDozer, &HydraulicBlade)>,
    building_stats: Res<BuildingStats>,
    debris_stats: Res<DebrisStats>,
    mut speed_text_query: Query<&mut Text, (With<HudSpeedText>, Without<HudBladeText>, Without<HudBuildingText>, Without<HudPerformanceText>)>,
    mut blade_text_query: Query<&mut Text, (With<HudBladeText>, Without<HudSpeedText>, Without<HudBuildingText>, Without<HudPerformanceText>)>,
    mut building_text_query: Query<&mut Text, (With<HudBuildingText>, Without<HudSpeedText>, Without<HudBladeText>, Without<HudPerformanceText>)>,
    mut perf_text_query: Query<&mut Text, (With<HudPerformanceText>, Without<HudSpeedText>, Without<HudBladeText>, Without<HudBuildingText>)>,
) {
    if let Ok((lin_vel, _dozer, blade)) = dozer_query.get_single() {
        let speed_ms = lin_vel.0.length();
        let speed_kmh = speed_ms * 3.6;
        let mass = 65_000.0;
        let kinetic_kj = 0.5 * mass * speed_ms * speed_ms / 1_000.0;

        if let Ok(mut text) = speed_text_query.get_single_mut() {
            text.0 = format!(
                "Speed: {:.1} m/s ({:.1} km/h)  |  Kinetic Energy: {:.0} kJ",
                speed_ms, speed_kmh, kinetic_kj
            );
        }

        if let Ok(mut text) = blade_text_query.get_single_mut() {
            let state = if blade.current_elevation < 0.05 {
                "GROUND SCRAPE"
            } else if blade.current_elevation > 1.75 {
                "MAX ELEVATED"
            } else {
                "INTERMEDIATE"
            };
            text.0 = format!(
                "Blade Elevation: {:.2}m / {:.2}m [{}]  |  Pitch: 15°",
                blade.current_elevation, blade.max_elevation, state
            );
        }
    }

    if let Ok(mut text) = building_text_query.get_single_mut() {
        text.0 = format!(
            "Intact Cells: {} / {}  |  Fractured: {}  |  Roof Integrity: {:.0}%",
            building_stats.intact_cells,
            building_stats.total_cells,
            building_stats.fractured_cells,
            building_stats.roof_intact_pct
        );
    }

    if let Ok(mut text) = perf_text_query.get_single_mut() {
        text.0 = format!(
            "Active Dynamic Debris: {} / {} Cap  |  Settled/Baked: {}",
            debris_stats.active_dynamic, debris_stats.max_cap, debris_stats.total_settled
        );
    }
}
