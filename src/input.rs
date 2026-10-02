use bevy::prelude::*;
use leafwing_input_manager::prelude::*;

#[derive(Actionlike, PartialEq, Eq, Clone, Copy, Hash, Debug, Reflect)]
pub enum PlayerAction {
    DriveForward,
    DriveBackward,
    TurnLeft,
    TurnRight,
    LeftTrackForward,
    LeftTrackBackward,
    RightTrackForward,
    RightTrackBackward,
    RaiseBlade,
    LowerBlade,
    ResetBuilding,
    ToggleView,
}

pub fn default_input_map() -> InputMap<PlayerAction> {
    InputMap::default()
        // Primary tank drive (WASD)
        .with(PlayerAction::DriveForward, KeyCode::KeyW)
        .with(PlayerAction::DriveForward, KeyCode::ArrowUp)
        .with(PlayerAction::DriveBackward, KeyCode::KeyS)
        .with(PlayerAction::DriveBackward, KeyCode::ArrowDown)
        .with(PlayerAction::TurnLeft, KeyCode::KeyA)
        .with(PlayerAction::TurnLeft, KeyCode::ArrowLeft)
        .with(PlayerAction::TurnRight, KeyCode::KeyD)
        .with(PlayerAction::TurnRight, KeyCode::ArrowRight)
        // Dedicated dual-track direct inputs (Q/A for Left Track, E/D for Right Track)
        .with(PlayerAction::LeftTrackForward, KeyCode::KeyQ)
        .with(PlayerAction::LeftTrackBackward, KeyCode::KeyZ)
        .with(PlayerAction::RightTrackForward, KeyCode::KeyE)
        .with(PlayerAction::RightTrackBackward, KeyCode::KeyC)
        // Hydraulic blade elevation (Space / Shift or R / F)
        .with(PlayerAction::RaiseBlade, KeyCode::Space)
        .with(PlayerAction::RaiseBlade, KeyCode::KeyR)
        .with(PlayerAction::LowerBlade, KeyCode::ShiftLeft)
        .with(PlayerAction::LowerBlade, KeyCode::KeyF)
        // Reset / rebuild building
        .with(PlayerAction::ResetBuilding, KeyCode::KeyB)
        // Camera perspective toggle
        .with(PlayerAction::ToggleView, KeyCode::KeyV)
        // Gamepad mappings
        .with(PlayerAction::DriveForward, GamepadButton::DPadUp)
        .with(PlayerAction::DriveBackward, GamepadButton::DPadDown)
        .with(PlayerAction::TurnLeft, GamepadButton::DPadLeft)
        .with(PlayerAction::TurnRight, GamepadButton::DPadRight)
        .with(PlayerAction::RaiseBlade, GamepadButton::RightTrigger2)
        .with(PlayerAction::LowerBlade, GamepadButton::LeftTrigger2)
        .with(PlayerAction::ResetBuilding, GamepadButton::North)
}
