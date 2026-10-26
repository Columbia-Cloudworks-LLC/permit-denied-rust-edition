//! `permit_denied`: Main binary entry point. Builds the windowed game from the
//! `permit_denied` library (simulation + presentation plugins).

use bevy::prelude::*;

fn main() {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "PERMIT DENIED — The County Said No. (Columbia Cloudworks LLC)".to_string(),
            resolution: (1280.0_f32, 720.0_f32).into(),
            present_mode: bevy::window::PresentMode::AutoVsync,
            fit_canvas_to_parent: true,
            ..Default::default()
        }),
        ..Default::default()
    }));
    permit_denied::add_simulation_plugins(&mut app);
    permit_denied::add_presentation_plugins(&mut app);
    app.run();
}
