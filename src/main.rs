//! `permit_denied`: Main binary entry point. Builds the windowed game from the
//! `permit_denied` library (simulation + presentation plugins), or runs a deterministic
//! scenario with `--scenario <file.ron> [--out <dir>] [--headless] [--write-expect]`.

use bevy::prelude::*;

fn main() -> AppExit {
    match permit_denied::scenario_cli::parse_args(std::env::args().skip(1)) {
        Ok(Some(args)) => return permit_denied::scenario_cli::run_cli(args),
        Ok(None) => {}
        Err(e) => {
            eprintln!("{e}");
            eprintln!("usage: permit_denied [--scenario <file.ron> [--out <dir>] [--headless] [--write-expect]]");
            return AppExit::from_code(2);
        }
    }

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
    permit_denied::add_debug_tooling_plugins(&mut app);
    app.run()
}
