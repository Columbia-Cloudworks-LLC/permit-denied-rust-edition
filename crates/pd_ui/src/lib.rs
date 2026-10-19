//! `pd_ui`: Exact visual parity Industrial HUD (Top Header Panel with 6-digit rolling mechanical
//! odometer reels, `MM:SS` amber LED clock, `Debug` & `Pause` buttons; Bottom Cab Dashboard with
//! corner flathead screws `⊖`, gold top trim, analog `ENGINE HEAT` & `TRACK STRESS` dial gauges,
//! `AR400 Edge` / `Charge-Air Kit` / `Aux Ram` bezel, and `BLADE UP`/`BLADE DOWN` LED) plus
//! interactive Title Screen ("COUNTY BUILDING DEPARTMENT - APPLICATION FOR DEMOLITION"), Pause,
//! Upgrade Selection (`1`/`2`/`3`), and New Game modals.
//!
//! Depends only on `pd_core` and `pd_dozer`.

use bevy::prelude::*;
use pd_core::{
    AppState, AudioSfxKind, AudioTriggerEvent, CashLedger, CashPopupPool, DebugTelemetry,
    ElapsedClock, EnvironmentConfig, GameMode, MAX_CASH_POPUPS, MAX_ODOMETER_CASH, PdSystemSet,
    SessionEpoch, SiteRestartRequested, sanitize_f32,
};
use pd_dozer::{
    BladeState, MechanicalStress, UpgradeChoice, UpgradeMultipliers, UpgradeProgression,
};
use serde::{Deserialize, Serialize};

/// Marker component for the root UI canvas spawned at startup.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct IndustrialHudRoot;

/// Marker component for each of the 6 individual rolling mechanical odometer reels (`0..6`).
#[derive(Component, Debug, Clone, Copy)]
pub struct OdometerReelCell {
    pub digit_index: usize,
}

/// Marker component for the formatted 6-digit mechanical odometer summary text.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct OdometerReadoutText;

/// Marker component for the 7-segment amber LED `MM:SS` clock readout.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct ElapsedClockReadoutText;

/// Marker component for the `ENGINE HEAT` gauge percentage readout.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct EngineHeatGaugeNode;

/// Marker component for the `TRACK STRESS` gauge percentage readout.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct TrackStressGaugeNode;

/// Marker component for the `AR400 Edge`, `Charge-Air Kit`, and `Aux Ram` multiplier bezel text.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct UpgradeClusterText;

/// Marker component for the `BLADE UP` / `BLADE DOWN` indicator text and LED dot.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct BladeStatusIndicatorText;

/// Marker component for the Title Screen ("COUNTY BUILDING DEPARTMENT - APPLICATION FOR DEMOLITION") modal.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct TitleScreenModalNode;

/// Marker component for the Pause Modal container.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct PauseModalNode;

/// Marker component for the interactive Milestone Upgrade Selection Modal container (`Issue #13`).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct UpgradeSelectionModalNode;

/// Marker component for the New Game configuration modal container.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct NewGameModalNode;

/// Marker component for the New Game modal dynamic configuration summary text.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct NewGameConfigSummaryText;

/// Marker component for the live Debug & Replay telemetry overlay panel.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct DebugOverlayPanelNode;

/// Marker component for the floating world-space cash popup summary feed.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct FloatingCashPopupBannerNode;

/// Marker component for the secondary info banner inside modals (`Controls`, `Sound`, `About`, `Facebook`).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct ModalSubInfoBannerText;

/// Interactive modal and HUD button actions.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HudButtonAction {
    PlayFromTitle,
    ToggleDebugOverlay,
    TogglePause,
    ResumeGame,
    RestartSite,
    OpenNewGameModal,
    ReturnToMainMenu,
    CycleGameMode,
    CycleCampaignLevel,
    CycleSeason,
    ToggleWeather,
    StartConfiguredGame,
    ShowControlsInfo,
    ToggleSoundMute,
    ShowAboutInfo,
    ShowSocialInfo,
    SelectUpgradeAr400Edge,
    SelectUpgradeChargeAirKit,
    SelectUpgradeAuxRam,
}

/// Computes a finite analog dial needle rotation angle (radians) for a `0%..=100%` gauge reading.
/// Maps `0%` to `-2.10 rad` (-120 deg) and `100%` to `+2.10 rad` (+120 deg).
#[must_use]
pub fn compute_gauge_needle_angle_radians(percentage: f32) -> f32 {
    let clamped_pct = sanitize_f32(percentage, 0.0).clamp(0.0, 100.0);
    let normalized = clamped_pct / 100.0;
    let angle = -2.10 + normalized * 4.20;
    sanitize_f32(angle, 0.0)
}

/// Renders an ASCII/Unicode dial needle indicator bar corresponding to the needle angle.
#[must_use]
pub fn needle_visual_glyph(percentage: f32) -> &'static str {
    let angle = compute_gauge_needle_angle_radians(percentage);
    if angle < -1.2 {
        "◜"
    } else if angle < -0.3 {
        "↖"
    } else if angle < 0.3 {
        "↑"
    } else if angle < 1.2 {
        "↗"
    } else {
        "◝"
    }
}

/// Formats the 6-digit mechanical odometer string (`$000,000` to `$999,999`) from `CashLedger`.
#[must_use]
pub fn format_odometer_string(ledger: &CashLedger) -> String {
    let display_val = sanitize_f32(ledger.odometer_display_value, 0.0)
        .round()
        .clamp(0.0, MAX_ODOMETER_CASH as f32) as u32;
    let thousands = display_val / 1_000;
    let hundreds = display_val % 1_000;
    format!("${thousands:03},{hundreds:03}")
}

fn spawn_beveled_button(
    parent: &mut ChildBuilder,
    label: &str,
    action: HudButtonAction,
    bg: Color,
    fg: Color,
) {
    parent
        .spawn((
            Button,
            action,
            Node {
                padding: UiRect::axes(Val::Px(12.0), Val::Px(6.0)),
                margin: UiRect::all(Val::Px(4.0)),
                border: UiRect::all(Val::Px(2.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..Default::default()
            },
            BackgroundColor(bg),
            BorderColor(Color::srgb(0.88, 0.68, 0.18)),
        ))
        .with_children(|btn| {
            btn.spawn((Text::new(label), TextColor(fg)));
        });
}

/// Spawns the industrial Top Header Bar, Bottom Cab Dashboard HUD, Floating Cash Popup layer,
/// Debug overlay, and Title / Pause / Upgrade Selection / New Game modals if not already present.
pub fn ensure_industrial_hud_spawned_system(
    mut commands: Commands,
    existing_root: Query<Entity, With<IndustrialHudRoot>>,
) {
    if existing_root.iter().next().is_some() {
        return;
    }

    commands
        .spawn((
            Name::new("Permit Denied Industrial HUD Root"),
            IndustrialHudRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                ..Default::default()
            },
        ))
        .with_children(|parent| {
            // =========================================================================
            // 1. TOP BAR (Industrial Header Panel)
            // =========================================================================
            parent
                .spawn((
                    Name::new("Top Industrial Header Panel"),
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Px(58.0),
                        padding: UiRect::axes(Val::Px(16.0), Val::Px(8.0)),
                        border: UiRect::bottom(Val::Px(2.0)),
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::Center,
                        ..Default::default()
                    },
                    BackgroundColor(Color::srgb(0.13, 0.14, 0.16)),
                    BorderColor(Color::srgb(0.86, 0.66, 0.16)),
                ))
                .with_children(|top| {
                    // Left: Stamped Form Badge
                    top.spawn((
                        Text::new("PERMIT DENIED - Application for Demolition"),
                        TextColor(Color::srgb(0.95, 0.78, 0.22)),
                    ));

                    // Center-Right: 6-Digit Mechanical Odometer Reels + Summary
                    top.spawn((
                        Node {
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(4.0),
                            ..Default::default()
                        },
                    ))
                    .with_children(|odo_row| {
                        odo_row.spawn((
                            OdometerReadoutText,
                            Text::new("CASH $000,000"),
                            TextColor(Color::srgb(0.98, 0.88, 0.32)),
                        ));
                        for digit_idx in 0..6_usize {
                            odo_row
                                .spawn((
                                    Node {
                                        width: Val::Px(18.0),
                                        height: Val::Px(26.0),
                                        border: UiRect::all(Val::Px(1.0)),
                                        justify_content: JustifyContent::Center,
                                        align_items: AlignItems::Center,
                                        ..Default::default()
                                    },
                                    BackgroundColor(Color::srgb(0.06, 0.07, 0.08)),
                                    BorderColor(Color::srgb(0.42, 0.44, 0.48)),
                                ))
                                .with_children(|cell| {
                                    cell.spawn((
                                        OdometerReelCell {
                                            digit_index: digit_idx,
                                        },
                                        Text::new("0"),
                                        TextColor(Color::srgb(0.98, 0.90, 0.38)),
                                    ));
                                });
                        }
                    });

                    // Right: Elapsed 7-Segment Amber LED Clock + Debug & Pause Beveled Buttons
                    top.spawn((
                        Node {
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(10.0),
                            ..Default::default()
                        },
                    ))
                    .with_children(|right_cluster| {
                        right_cluster.spawn((
                            ElapsedClockReadoutText,
                            Text::new("ELAPSED 00:00"),
                            TextColor(Color::srgb(1.0, 0.68, 0.14)),
                        ));
                        spawn_beveled_button(
                            right_cluster,
                            "Debug",
                            HudButtonAction::ToggleDebugOverlay,
                            Color::srgb(0.20, 0.22, 0.25),
                            Color::srgb(0.88, 0.90, 0.92),
                        );
                        spawn_beveled_button(
                            right_cluster,
                            "Pause",
                            HudButtonAction::TogglePause,
                            Color::srgb(0.20, 0.22, 0.25),
                            Color::srgb(0.96, 0.80, 0.24),
                        );
                    });
                });

            // =========================================================================
            // 2. CENTER VIEWPORT & MODAL OVERLAY CONTAINER
            // =========================================================================
            parent
                .spawn((
                    Name::new("Center Viewport & Modal Overlay"),
                    Node {
                        width: Val::Percent(100.0),
                        flex_grow: 1.0,
                        flex_direction: FlexDirection::Column,
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..Default::default()
                    },
                ))
                .with_children(|center| {
                    // Floating Cash Popups (`+$5`, `+$16`, `+$71`, `+$89`)
                    center.spawn((
                        FloatingCashPopupBannerNode,
                        Text::new(""),
                        TextColor(Color::srgb(1.0, 0.92, 0.24)),
                    ));

                    // Live Debug Overlay Panel (Hidden unless toggled)
                    center.spawn((
                        DebugOverlayPanelNode,
                        Text::new(""),
                        TextColor(Color::srgb(0.55, 0.96, 0.62)),
                    ));

                    // -----------------------------------------------------------------
                    // Modal A: Title Screen ("COUNTY BUILDING DEPARTMENT - APPLICATION FOR DEMOLITION")
                    // -----------------------------------------------------------------
                    center
                        .spawn((
                            Name::new("Title Screen Permit Application Modal"),
                            TitleScreenModalNode,
                            Node {
                                width: Val::Px(620.0),
                                padding: UiRect::all(Val::Px(22.0)),
                                border: UiRect::all(Val::Px(3.0)),
                                flex_direction: FlexDirection::Column,
                                align_items: AlignItems::Center,
                                row_gap: Val::Px(10.0),
                                ..Default::default()
                            },
                            BackgroundColor(Color::srgb(0.95, 0.92, 0.84)), // Cream paper form
                            BorderColor(Color::srgb(0.22, 0.20, 0.18)),
                        ))
                        .with_children(|form| {
                            form.spawn((
                                Text::new(
                                    "COUNTY BUILDING DEPARTMENT - APPLICATION FOR DEMOLITION",
                                ),
                                TextColor(Color::srgb(0.16, 0.16, 0.18)),
                            ));
                            form.spawn((
                                Text::new("[ STAMPED: PERMIT DENIED ] — \"The County Said No.\""),
                                TextColor(Color::srgb(0.82, 0.14, 0.14)), // Red stamp ink
                            ));

                            spawn_beveled_button(
                                form,
                                "⏻ PLAY",
                                HudButtonAction::PlayFromTitle,
                                Color::srgb(0.90, 0.68, 0.14),
                                Color::srgb(0.10, 0.10, 0.12),
                            );

                            form.spawn((
                                Node {
                                    flex_direction: FlexDirection::Row,
                                    justify_content: JustifyContent::Center,
                                    column_gap: Val::Px(8.0),
                                    ..Default::default()
                                },
                            ))
                            .with_children(|grid| {
                                spawn_beveled_button(
                                    grid,
                                    "New Game",
                                    HudButtonAction::OpenNewGameModal,
                                    Color::srgb(0.24, 0.26, 0.28),
                                    Color::srgb(0.95, 0.82, 0.28),
                                );
                                spawn_beveled_button(
                                    grid,
                                    "Controls",
                                    HudButtonAction::ShowControlsInfo,
                                    Color::srgb(0.24, 0.26, 0.28),
                                    Color::WHITE,
                                );
                                spawn_beveled_button(
                                    grid,
                                    "Sound",
                                    HudButtonAction::ToggleSoundMute,
                                    Color::srgb(0.24, 0.26, 0.28),
                                    Color::WHITE,
                                );
                                spawn_beveled_button(
                                    grid,
                                    "About",
                                    HudButtonAction::ShowAboutInfo,
                                    Color::srgb(0.24, 0.26, 0.28),
                                    Color::WHITE,
                                );
                                spawn_beveled_button(
                                    grid,
                                    "Facebook",
                                    HudButtonAction::ShowSocialInfo,
                                    Color::srgb(0.20, 0.34, 0.58),
                                    Color::WHITE,
                                );
                            });

                            form.spawn((
                                ModalSubInfoBannerText,
                                Text::new(
                                    "WASD/Arrows: Drive | Space/F/R/Shift: Hold Blade (Powered Push) | 1/2/3: Upgrade | B: Restart",
                                ),
                                TextColor(Color::srgb(0.25, 0.24, 0.22)),
                            ));

                            form.spawn((
                                Text::new("Columbia Cloudworks LLC — All Rights Reserved"),
                                TextColor(Color::srgb(0.36, 0.34, 0.32)),
                            ));
                        });

                    // -----------------------------------------------------------------
                    // Modal B: Pause Modal (`Resume`, `Restart Site`, `New Game`, `Main Menu`)
                    // -----------------------------------------------------------------
                    center
                        .spawn((
                            Name::new("Pause Modal"),
                            PauseModalNode,
                            Node {
                                width: Val::Px(460.0),
                                padding: UiRect::all(Val::Px(20.0)),
                                border: UiRect::all(Val::Px(2.0)),
                                flex_direction: FlexDirection::Column,
                                align_items: AlignItems::Center,
                                row_gap: Val::Px(8.0),
                                display: Display::None,
                                ..Default::default()
                            },
                            BackgroundColor(Color::srgb(0.15, 0.16, 0.18)),
                            BorderColor(Color::srgb(0.88, 0.68, 0.18)),
                        ))
                        .with_children(|pause| {
                            pause.spawn((
                                Text::new("SITE OPERATIONS PAUSED"),
                                TextColor(Color::srgb(0.96, 0.80, 0.24)),
                            ));
                            spawn_beveled_button(
                                pause,
                                "Resume",
                                HudButtonAction::ResumeGame,
                                Color::srgb(0.90, 0.68, 0.14),
                                Color::srgb(0.10, 0.10, 0.12),
                            );
                            spawn_beveled_button(
                                pause,
                                "Restart Site",
                                HudButtonAction::RestartSite,
                                Color::srgb(0.24, 0.26, 0.30),
                                Color::WHITE,
                            );
                            spawn_beveled_button(
                                pause,
                                "New Game",
                                HudButtonAction::OpenNewGameModal,
                                Color::srgb(0.24, 0.26, 0.30),
                                Color::WHITE,
                            );
                            spawn_beveled_button(
                                pause,
                                "Main Menu",
                                HudButtonAction::ReturnToMainMenu,
                                Color::srgb(0.24, 0.26, 0.30),
                                Color::srgb(0.94, 0.56, 0.52),
                            );
                        });

                    // -----------------------------------------------------------------
                    // Modal B2: Milestone Upgrade Selection Modal (`1`/`2`/`3` Choice)
                    // -----------------------------------------------------------------
                    center
                        .spawn((
                            Name::new("Equipment Upgrade Selection Modal"),
                            UpgradeSelectionModalNode,
                            Node {
                                width: Val::Px(520.0),
                                padding: UiRect::all(Val::Px(20.0)),
                                border: UiRect::all(Val::Px(2.0)),
                                flex_direction: FlexDirection::Column,
                                align_items: AlignItems::Center,
                                row_gap: Val::Px(8.0),
                                display: Display::None,
                                ..Default::default()
                            },
                            BackgroundColor(Color::srgb(0.15, 0.16, 0.18)),
                            BorderColor(Color::srgb(0.94, 0.76, 0.22)),
                        ))
                        .with_children(|up_modal| {
                            up_modal.spawn((
                                Text::new("EQUIPMENT UPGRADE MILESTONE — CHOOSE AN UPGRADE"),
                                TextColor(Color::srgb(0.96, 0.80, 0.24)),
                            ));
                            spawn_beveled_button(
                                up_modal,
                                UpgradeChoice::Ar400Edge.label(),
                                HudButtonAction::SelectUpgradeAr400Edge,
                                Color::srgb(0.24, 0.28, 0.32),
                                Color::srgb(0.98, 0.86, 0.32),
                            );
                            spawn_beveled_button(
                                up_modal,
                                UpgradeChoice::ChargeAirKit.label(),
                                HudButtonAction::SelectUpgradeChargeAirKit,
                                Color::srgb(0.24, 0.28, 0.32),
                                Color::srgb(0.98, 0.86, 0.32),
                            );
                            spawn_beveled_button(
                                up_modal,
                                UpgradeChoice::AuxRam.label(),
                                HudButtonAction::SelectUpgradeAuxRam,
                                Color::srgb(0.24, 0.28, 0.32),
                                Color::srgb(0.98, 0.86, 0.32),
                            );
                        });

                    // -----------------------------------------------------------------
                    // Modal C: New Game Modal (`Mode`, `Level`, `Season`, `Weather`)
                    // -----------------------------------------------------------------
                    center
                        .spawn((
                            Name::new("New Game Configuration Modal"),
                            NewGameModalNode,
                            Node {
                                width: Val::Px(540.0),
                                padding: UiRect::all(Val::Px(20.0)),
                                border: UiRect::all(Val::Px(2.0)),
                                flex_direction: FlexDirection::Column,
                                align_items: AlignItems::Center,
                                row_gap: Val::Px(8.0),
                                display: Display::None,
                                ..Default::default()
                            },
                            BackgroundColor(Color::srgb(0.15, 0.16, 0.18)),
                            BorderColor(Color::srgb(0.88, 0.68, 0.18)),
                        ))
                        .with_children(|ng| {
                            ng.spawn((
                                Text::new("NEW DEMOLITION SITE CONFIGURATION"),
                                TextColor(Color::srgb(0.96, 0.80, 0.24)),
                            ));
                            ng.spawn((
                                NewGameConfigSummaryText,
                                Text::new(
                                    "Mode: Campaign | Level: 1/7 County | Season: Random | Weather: On",
                                ),
                                TextColor(Color::WHITE),
                            ));
                            ng.spawn((
                                Node {
                                    flex_direction: FlexDirection::Row,
                                    column_gap: Val::Px(6.0),
                                    ..Default::default()
                                },
                            ))
                            .with_children(|selectors| {
                                spawn_beveled_button(
                                    selectors,
                                    "Mode",
                                    HudButtonAction::CycleGameMode,
                                    Color::srgb(0.22, 0.24, 0.28),
                                    Color::WHITE,
                                );
                                spawn_beveled_button(
                                    selectors,
                                    "Level (1/7-7/7)",
                                    HudButtonAction::CycleCampaignLevel,
                                    Color::srgb(0.22, 0.24, 0.28),
                                    Color::WHITE,
                                );
                                spawn_beveled_button(
                                    selectors,
                                    "Season",
                                    HudButtonAction::CycleSeason,
                                    Color::srgb(0.22, 0.24, 0.28),
                                    Color::WHITE,
                                );
                                spawn_beveled_button(
                                    selectors,
                                    "Weather",
                                    HudButtonAction::ToggleWeather,
                                    Color::srgb(0.22, 0.24, 0.28),
                                    Color::WHITE,
                                );
                            });
                            ng.spawn((
                                Node {
                                    flex_direction: FlexDirection::Row,
                                    column_gap: Val::Px(12.0),
                                    ..Default::default()
                                },
                            ))
                            .with_children(|actions| {
                                spawn_beveled_button(
                                    actions,
                                    "Start Game",
                                    HudButtonAction::StartConfiguredGame,
                                    Color::srgb(0.90, 0.68, 0.14),
                                    Color::srgb(0.10, 0.10, 0.12),
                                );
                                spawn_beveled_button(
                                    actions,
                                    "Back",
                                    HudButtonAction::ReturnToMainMenu,
                                    Color::srgb(0.26, 0.28, 0.30),
                                    Color::WHITE,
                                );
                            });
                        });
                });

            // =========================================================================
            // 3. BOTTOM BAR (Cab Dashboard Panel with corner screws ⊖ and gold top trim)
            // =========================================================================
            parent
                .spawn((
                    Name::new("Bottom Cab Dashboard Panel"),
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Px(80.0),
                        padding: UiRect::axes(Val::Px(16.0), Val::Px(10.0)),
                        border: UiRect::top(Val::Px(3.0)),
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::Center,
                        ..Default::default()
                    },
                    BackgroundColor(Color::srgb(0.11, 0.12, 0.14)),
                    BorderColor(Color::srgb(0.90, 0.70, 0.18)), // Gold top trim border
                ))
                .with_children(|bottom| {
                    bottom.spawn((
                        EngineHeatGaugeNode,
                        Text::new("⊖ ENGINE HEAT [◜] 000%"),
                        TextColor(Color::srgb(0.94, 0.76, 0.24)),
                    ));
                    bottom.spawn((
                        TrackStressGaugeNode,
                        Text::new("TRACK STRESS [◜] 000%"),
                        TextColor(Color::srgb(0.94, 0.76, 0.24)),
                    ));
                    bottom.spawn((
                        UpgradeClusterText,
                        Text::new("AR400 Edge 1.00x | Charge-Air Kit 1.00x | Aux Ram 1.00x"),
                        TextColor(Color::srgb(0.86, 0.88, 0.90)),
                    ));
                    bottom.spawn((
                        BladeStatusIndicatorText,
                        Text::new("● BLADE UP ⊖"),
                        TextColor(Color::srgb(0.45, 0.88, 0.48)),
                    ));
                });
        });
}

/// Handles mouse clicks on all beveled HUD and Modal buttons (`HudButtonAction`).
#[allow(clippy::too_many_arguments)]
pub fn handle_hud_button_interactions_system(
    interactions: Query<(&Interaction, &HudButtonAction), Changed<Interaction>>,
    mut next_state: ResMut<NextState<AppState>>,
    app_state: Res<State<AppState>>,
    mut env_config: ResMut<EnvironmentConfig>,
    mut upgrades: ResMut<UpgradeMultipliers>,
    mut progression: ResMut<UpgradeProgression>,
    mut telemetry: ResMut<DebugTelemetry>,
    mut restart_writer: EventWriter<SiteRestartRequested>,
    mut audio_writer: EventWriter<AudioTriggerEvent>,
) {
    for (interaction, action) in &interactions {
        if *interaction != Interaction::Pressed {
            continue;
        }

        audio_writer.send(AudioTriggerEvent {
            kind: AudioSfxKind::CashDing,
        });

        match *action {
            HudButtonAction::PlayFromTitle | HudButtonAction::StartConfiguredGame => {
                progression.reset_new_game(&mut upgrades, env_config.level);
                next_state.set(AppState::Playing);
                restart_writer.send(SiteRestartRequested {
                    preserve_layout: false,
                });
            }
            HudButtonAction::ToggleDebugOverlay => {
                telemetry.debug_overlay_visible = !telemetry.debug_overlay_visible;
            }
            HudButtonAction::TogglePause => match app_state.get() {
                AppState::Playing => next_state.set(AppState::Paused),
                AppState::Paused => {
                    if !(env_config.mode == GameMode::Campaign
                        && progression.pending_upgrade_choices > 0)
                    {
                        next_state.set(AppState::Playing);
                    }
                }
                AppState::Title | AppState::NewGameModal => {}
            },
            HudButtonAction::ResumeGame => {
                if !(env_config.mode == GameMode::Campaign
                    && progression.pending_upgrade_choices > 0)
                {
                    next_state.set(AppState::Playing);
                }
            }
            HudButtonAction::RestartSite => {
                if !(env_config.mode == GameMode::Campaign
                    && progression.pending_upgrade_choices > 0)
                {
                    next_state.set(AppState::Playing);
                    restart_writer.send(SiteRestartRequested {
                        preserve_layout: true,
                    });
                }
            }
            HudButtonAction::OpenNewGameModal => {
                if !(*app_state.get() == AppState::Paused
                    && env_config.mode == GameMode::Campaign
                    && progression.pending_upgrade_choices > 0)
                {
                    next_state.set(AppState::NewGameModal);
                }
            }
            HudButtonAction::ReturnToMainMenu => {
                if !(*app_state.get() == AppState::Paused
                    && env_config.mode == GameMode::Campaign
                    && progression.pending_upgrade_choices > 0)
                {
                    next_state.set(AppState::Title);
                }
            }
            HudButtonAction::CycleGameMode => {
                env_config.mode = match env_config.mode {
                    GameMode::Campaign => GameMode::Sandbox,
                    GameMode::Sandbox => GameMode::Campaign,
                };
            }
            HudButtonAction::CycleCampaignLevel => {
                env_config.level = env_config.level.next();
            }
            HudButtonAction::CycleSeason => {
                env_config.season = env_config.season.next();
            }
            HudButtonAction::ToggleWeather => {
                env_config.weather = env_config.weather.toggle();
            }
            HudButtonAction::ShowControlsInfo => {
                telemetry.info_banner = Some(
                    "WASD/Arrows: Skid-Steer | Space/F/R/Shift: Hold Blade (Powered Push) | 1/2/3: Upgrade | B: Restart".to_string(),
                );
            }
            HudButtonAction::ToggleSoundMute => {
                telemetry.sound_enabled = !telemetry.sound_enabled;
                let status = if telemetry.sound_enabled { "ON" } else { "MUTED" };
                telemetry.info_banner = Some(format!("Industrial Diesel & Crunch Audio: {status}"));
            }
            HudButtonAction::ShowAboutInfo => {
                telemetry.info_banner = Some(
                    "PERMIT DENIED (Rust/Bevy 0.15 + Avian2D) — Columbia Cloudworks LLC (\"The County Said No.\")".to_string(),
                );
            }
            HudButtonAction::ShowSocialInfo => {
                telemetry.info_banner = Some(
                    "Columbia Cloudworks LLC Municipal Demolition Division — Permit #000-DENIED".to_string(),
                );
            }
            HudButtonAction::SelectUpgradeAr400Edge => {
                if progression.try_apply_choice(
                    &mut upgrades,
                    env_config.mode,
                    UpgradeChoice::Ar400Edge,
                ) && env_config.mode == GameMode::Campaign
                    && progression.pending_upgrade_choices == 0
                {
                    next_state.set(AppState::Playing);
                }
            }
            HudButtonAction::SelectUpgradeChargeAirKit => {
                if progression.try_apply_choice(
                    &mut upgrades,
                    env_config.mode,
                    UpgradeChoice::ChargeAirKit,
                ) && env_config.mode == GameMode::Campaign
                    && progression.pending_upgrade_choices == 0
                {
                    next_state.set(AppState::Playing);
                }
            }
            HudButtonAction::SelectUpgradeAuxRam => {
                if progression.try_apply_choice(
                    &mut upgrades,
                    env_config.mode,
                    UpgradeChoice::AuxRam,
                ) && env_config.mode == GameMode::Campaign
                    && progression.pending_upgrade_choices == 0
                {
                    next_state.set(AppState::Playing);
                }
            }
        }
    }
}

/// Synchronizes modal visibility (`TitleScreenModalNode`, `PauseModalNode`,
/// `UpgradeSelectionModalNode`, `NewGameModalNode`) with `AppState` and `UpgradeProgression`.
#[allow(clippy::type_complexity)]
pub fn sync_modal_visibility_system(
    app_state: Res<State<AppState>>,
    env_config: Res<EnvironmentConfig>,
    progression: Res<UpgradeProgression>,
    telemetry: Res<DebugTelemetry>,
    mut title_q: Query<
        &mut Node,
        (
            With<TitleScreenModalNode>,
            Without<PauseModalNode>,
            Without<UpgradeSelectionModalNode>,
            Without<NewGameModalNode>,
        ),
    >,
    mut pause_q: Query<
        &mut Node,
        (
            With<PauseModalNode>,
            Without<TitleScreenModalNode>,
            Without<UpgradeSelectionModalNode>,
            Without<NewGameModalNode>,
        ),
    >,
    mut upgrade_q: Query<
        &mut Node,
        (
            With<UpgradeSelectionModalNode>,
            Without<TitleScreenModalNode>,
            Without<PauseModalNode>,
            Without<NewGameModalNode>,
        ),
    >,
    mut newgame_q: Query<
        &mut Node,
        (
            With<NewGameModalNode>,
            Without<TitleScreenModalNode>,
            Without<PauseModalNode>,
            Without<UpgradeSelectionModalNode>,
        ),
    >,
    mut config_text_q: Query<
        &mut Text,
        (
            With<NewGameConfigSummaryText>,
            Without<ModalSubInfoBannerText>,
        ),
    >,
    mut info_banner_q: Query<
        &mut Text,
        (
            With<ModalSubInfoBannerText>,
            Without<NewGameConfigSummaryText>,
        ),
    >,
) {
    let state = *app_state.get();
    let show_upgrade_modal = state == AppState::Paused
        && env_config.mode == GameMode::Campaign
        && progression.pending_upgrade_choices > 0;

    if let Ok(mut node) = title_q.get_single_mut() {
        node.display = if state == AppState::Title {
            Display::Flex
        } else {
            Display::None
        };
    }
    if let Ok(mut node) = pause_q.get_single_mut() {
        node.display = if state == AppState::Paused && !show_upgrade_modal {
            Display::Flex
        } else {
            Display::None
        };
    }
    if let Ok(mut node) = upgrade_q.get_single_mut() {
        node.display = if show_upgrade_modal {
            Display::Flex
        } else {
            Display::None
        };
    }
    if let Ok(mut node) = newgame_q.get_single_mut() {
        node.display = if state == AppState::NewGameModal {
            Display::Flex
        } else {
            Display::None
        };
    }

    if let Ok(mut txt) = config_text_q.get_single_mut() {
        txt.0 = format!(
            "Mode: {} | Level: {} | Season: {} | Weather: {}",
            env_config.mode.label(),
            env_config.level.label(),
            env_config.season.label(),
            env_config.weather.label()
        );
    }

    if let Some(banner) = &telemetry.info_banner
        && let Ok(mut info_txt) = info_banner_q.get_single_mut()
    {
        info_txt.0.clone_from(banner);
    }
}

/// Updates the Top/Bottom HUD readouts (6-digit rolling odometer reels, `MM:SS` clock, analog gauge
/// needles, upgrade multipliers, blade LED status, floating cash popups, and debug telemetry).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_industrial_hud_system(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    app_state: Res<State<AppState>>,
    mut next_state: ResMut<NextState<AppState>>,
    mut env_config: ResMut<EnvironmentConfig>,
    epoch: Res<SessionEpoch>,
    ledger: Res<CashLedger>,
    popup_pool: Res<CashPopupPool>,
    clock: Res<ElapsedClock>,
    blade_state: Res<BladeState>,
    mut upgrades: ResMut<UpgradeMultipliers>,
    mut progression: ResMut<UpgradeProgression>,
    stress: Res<MechanicalStress>,
    mut telemetry: ResMut<DebugTelemetry>,
    mut restart_writer: EventWriter<SiteRestartRequested>,
    mut audio_writer: EventWriter<AudioTriggerEvent>,
    mut hud_texts: Query<(
        &mut Text,
        Option<&mut TextColor>,
        Option<&OdometerReelCell>,
        Has<OdometerReadoutText>,
        Has<ElapsedClockReadoutText>,
        Has<EngineHeatGaugeNode>,
        Has<TrackStressGaugeNode>,
        Has<UpgradeClusterText>,
        Has<BladeStatusIndicatorText>,
        Has<DebugOverlayPanelNode>,
        Has<FloatingCashPopupBannerNode>,
    )>,
) {
    if let Some(keys) = keys {
        let has_pending_campaign_upgrade =
            env_config.mode == GameMode::Campaign && progression.pending_upgrade_choices > 0;
        if keys.just_pressed(KeyCode::Enter)
            && (*app_state.get() == AppState::Title || *app_state.get() == AppState::NewGameModal)
        {
            progression.reset_new_game(&mut upgrades, env_config.level);
            next_state.set(AppState::Playing);
            restart_writer.send(SiteRestartRequested {
                preserve_layout: false,
            });
            audio_writer.send(AudioTriggerEvent {
                kind: AudioSfxKind::CashDing,
            });
        }
        if keys.just_pressed(KeyCode::Escape) {
            match app_state.get() {
                AppState::Playing => next_state.set(AppState::Paused),
                AppState::Paused => {
                    if !has_pending_campaign_upgrade {
                        next_state.set(AppState::Playing);
                    }
                }
                AppState::Title | AppState::NewGameModal => {}
            }
        }
        if keys.just_pressed(KeyCode::F3) {
            telemetry.debug_overlay_visible = !telemetry.debug_overlay_visible;
        }
        if keys.just_pressed(KeyCode::KeyB) && !has_pending_campaign_upgrade {
            restart_writer.send(SiteRestartRequested {
                preserve_layout: true,
            });
        }
        if keys.just_pressed(KeyCode::Tab) && !has_pending_campaign_upgrade {
            env_config.level = env_config.level.next();
            restart_writer.send(SiteRestartRequested {
                preserve_layout: false,
            });
        }
    }

    let reel_digits = ledger.odometer_reel_digits();
    let heat_pct = sanitize_f32(stress.engine_heat_pct, 0.0).clamp(0.0, 100.0);
    let heat_glyph = needle_visual_glyph(heat_pct);
    let track_pct = sanitize_f32(stress.track_stress_pct, 0.0).clamp(0.0, 100.0);
    let track_glyph = needle_visual_glyph(track_pct);
    let safe_up = upgrades.sanitized();

    for (
        mut text,
        maybe_color,
        maybe_reel,
        is_odo,
        is_clock,
        is_heat,
        is_stress,
        is_upgrade,
        is_blade,
        is_debug,
        is_popup_banner,
    ) in &mut hud_texts
    {
        if let Some(cell) = maybe_reel {
            if let Some(&reel_val) = reel_digits.get(cell.digit_index) {
                let d = (sanitize_f32(reel_val, 0.0).floor() as u32) % 10;
                text.0 = format!("{d}");
            }
        } else if is_odo {
            text.0 = format!(
                "[{}] CASH {}",
                env_config.level.label(),
                format_odometer_string(&ledger)
            );
        } else if is_clock {
            text.0 = format!("ELAPSED {}", clock.format_mm_ss());
        } else if is_heat {
            text.0 = format!("⊖ ENGINE HEAT [{heat_glyph}] {:03.0}%", heat_pct);
        } else if is_stress {
            text.0 = format!("TRACK STRESS [{track_glyph}] {:03.0}%", track_pct);
        } else if is_upgrade {
            if env_config.mode == GameMode::Campaign && progression.pending_upgrade_choices > 0 {
                text.0 = format!(
                    "[1] AR400 {:.2}x | [2] Charge-Air {:.2}x | [3] Aux Ram {:.2}x (PICK UPGRADE!)",
                    safe_up.ar400_edge, safe_up.charge_air_kit, safe_up.aux_ram
                );
            } else {
                text.0 = format!(
                    "AR400 Edge {:.2}x | Charge-Air Kit {:.2}x | Aux Ram {:.2}x",
                    safe_up.ar400_edge, safe_up.charge_air_kit, safe_up.aux_ram
                );
            }
        } else if is_blade {
            text.0 = format!("● {} ⊖", blade_state.label());
            if let Some(mut color) = maybe_color {
                *color = match *blade_state {
                    BladeState::BladeUp => TextColor(Color::srgb(0.45, 0.88, 0.48)),
                    BladeState::BladeDown => TextColor(Color::srgb(1.0, 0.72, 0.16)),
                };
            }
        } else if is_popup_banner {
            let mut active_labels = String::new();
            for slot in popup_pool.slots.iter().filter(|s| s.active).take(6) {
                if !active_labels.is_empty() {
                    active_labels.push_str("   ");
                }
                active_labels.push_str(&format!("+${}", slot.amount));
            }
            text.0 = active_labels;
        } else if is_debug {
            if telemetry.debug_overlay_visible {
                text.0 = format!(
                    "DEBUG TELEMETRY | Epoch: {} | Active Debris: {}/500 | Sleeping Rubble: {} | Crushed Decals: {} | Cash Popups: {}/{} | Intact Nodes: {} | BFS Tasks: {} | Panicked NPCs: {}",
                    epoch.0,
                    telemetry.active_physics_debris,
                    telemetry.sleeping_rubble_count,
                    telemetry.crushed_decal_count,
                    popup_pool.active_count(),
                    MAX_CASH_POPUPS,
                    telemetry.intact_structural_nodes,
                    telemetry.active_bfs_tasks,
                    telemetry.panicked_pedestrians,
                );
            } else {
                text.0.clear();
            }
        }
    }
}

/// Bevy plugin for `pd_ui`.
pub struct PdUiPlugin;

impl Plugin for PdUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                ensure_industrial_hud_spawned_system,
                handle_hud_button_interactions_system,
                sync_modal_visibility_system,
                update_industrial_hud_system,
            )
                .chain()
                .in_set(PdSystemSet::HudInterpolation),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::state::app::StatesPlugin;
    use pd_core::PdCorePlugin;
    use pd_dozer::PdDozerPlugin;

    #[test]
    fn test_gauge_needle_and_odometer_formatting_extremes() {
        assert!(compute_gauge_needle_angle_radians(f32::NAN).is_finite());
        assert!(compute_gauge_needle_angle_radians(f32::INFINITY).is_finite());
        assert!(compute_gauge_needle_angle_radians(-999.0).is_finite());

        let mut ledger = CashLedger {
            current_cash: 999_999,
            lifetime_cash: 999_999,
            odometer_display_value: 999_999.0,
        };
        assert_eq!(format_odometer_string(&ledger), "$999,999");

        ledger.odometer_display_value = f32::NAN;
        assert_eq!(format_odometer_string(&ledger), "$000,000");
    }

    #[test]
    fn test_pending_campaign_upgrade_guards_escape_and_restart_shortcuts() {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            PdCorePlugin,
            PdDozerPlugin,
            PdUiPlugin,
        ));
        app.init_resource::<ButtonInput<KeyCode>>();
        app.update();

        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Paused);
        app.update();

        let initial_level = pd_core::CampaignLevel::default();
        {
            let mut env = app.world_mut().resource_mut::<EnvironmentConfig>();
            env.mode = GameMode::Campaign;
            env.level = initial_level;
        }
        {
            let mut prog = app.world_mut().resource_mut::<UpgradeProgression>();
            prog.pending_upgrade_choices = 1;
        }

        // Pressing Escape, B (retry), or Tab (level advance) while an upgrade is pending must be ignored
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.press(KeyCode::Escape);
            keys.press(KeyCode::KeyB);
            keys.press(KeyCode::Tab);
        }
        app.update();
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.release(KeyCode::Escape);
            keys.release(KeyCode::KeyB);
            keys.release(KeyCode::Tab);
            keys.clear();
        }
        app.update();

        assert_eq!(
            *app.world().resource::<State<AppState>>().get(),
            AppState::Paused,
            "Campaign upgrade modal must remain Paused when Escape/B/Tab are pressed"
        );
        assert_eq!(
            app.world()
                .resource::<UpgradeProgression>()
                .pending_upgrade_choices,
            1,
            "Pending upgrade choice must not be cleared by B/Tab shortcuts"
        );
        assert_eq!(
            app.world().resource::<EnvironmentConfig>().level,
            initial_level,
            "Tab shortcut must not advance level while an upgrade choice is pending"
        );
    }
}
