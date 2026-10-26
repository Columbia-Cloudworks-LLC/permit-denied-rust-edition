//! `permit-denied-scenario` v1: a deterministic run description stored as RON.
//!
//! Mirrors the reference's `permit-denied-playthrough` v1 (setup, change-only control stream,
//! tick-stamped discrete actions, end-state digest) and adds capture ticks, overlay flags and
//! an exit tick so the same file drives CLI captures, regression tests and in-app replays.

use crate::digest::Digest;
use pd_core::{CampaignLevel, GameMode, Season, SiteTestMap, WeatherSetting};
use pd_dozer::{UpgradeChoice, UpgradeMultipliers};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

/// `format` tag every scenario file carries.
pub const SCENARIO_FORMAT: &str = "permit-denied-scenario";

/// Current scenario schema version.
pub const SCENARIO_VERSION: u32 = 1;

/// Upper bound on `exit_tick` (one hour at 60 Hz) so a corrupt file cannot run forever.
pub const MAX_SCENARIO_TICKS: u64 = 60 * 60 * 60;

/// Track/blade control for one tick: `left_track`, `right_track` in `-2.0..=2.0`, blade held.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct DriveControl {
    pub left: f32,
    pub right: f32,
    pub blade: bool,
}

/// Change-only control entry `(tick, left, right, blade)`, held until the next entry.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ControlEntry(pub u64, pub f32, pub f32, pub bool);

impl ControlEntry {
    #[must_use]
    pub const fn control(&self) -> DriveControl {
        DriveControl {
            left: self.1,
            right: self.2,
            blade: self.3,
        }
    }
}

/// Discrete action applied before tick `at` runs (`at` = ticks already completed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScenarioAction {
    Upgrade { at: u64, choice: UpgradeChoice },
}

impl ScenarioAction {
    #[must_use]
    pub const fn at(&self) -> u64 {
        match self {
            Self::Upgrade { at, .. } => *at,
        }
    }
}

/// Screenshot + telemetry capture taken once `tick` ticks have completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureSpec {
    pub tick: u64,
    pub label: String,
}

/// Debug overlays shown during a scenario run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Overlays {
    /// F3 telemetry panel.
    #[serde(default)]
    pub telemetry: bool,
    /// Avian2D collider outlines projected into the isometric view (F4).
    #[serde(default)]
    pub colliders: bool,
}

/// Session setup that rebuilds the site.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioSettings {
    pub mode: GameMode,
    pub level: CampaignLevel,
    pub season: Season,
    pub weather: WeatherSetting,
    #[serde(default)]
    pub test_map: SiteTestMap,
    /// Installed upgrades at tick 0.
    #[serde(default)]
    pub upgrades: UpgradeMultipliers,
}

/// A deterministic run: setup, inputs, captures, exit tick and (optionally) the expected digest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub name: String,
    /// Build that produced `expect`; a different build may legitimately drift.
    #[serde(default)]
    pub build: String,
    #[serde(default)]
    pub recorded_at: Option<String>,
    pub seed: u64,
    pub settings: ScenarioSettings,
    #[serde(default)]
    pub controls: Vec<ControlEntry>,
    #[serde(default)]
    pub actions: Vec<ScenarioAction>,
    #[serde(default)]
    pub captures: Vec<CaptureSpec>,
    #[serde(default)]
    pub overlays: Overlays,
    /// Ticks to run before judging the end state.
    pub exit_tick: u64,
    /// Expected end state; `None` runs without a MATCH/DRIFT verdict.
    #[serde(default)]
    pub expect: Option<Digest>,
}

/// Why a scenario could not be loaded, saved or accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenarioError {
    Io(String),
    Parse(String),
    Format(String),
    Invalid(String),
}

impl fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "scenario I/O error: {msg}"),
            Self::Parse(msg) => write!(f, "scenario is not valid RON: {msg}"),
            Self::Format(msg) => write!(f, "not a PERMIT DENIED scenario: {msg}"),
            Self::Invalid(msg) => write!(f, "scenario is invalid: {msg}"),
        }
    }
}

impl std::error::Error for ScenarioError {}

/// Build identifier stamped into recordings.
#[must_use]
pub fn build_id() -> String {
    format!("permit_denied {}", env!("CARGO_PKG_VERSION"))
}

impl Scenario {
    /// Empty scenario for `settings` and `seed` (no inputs, exits at tick 0).
    #[must_use]
    pub fn new(name: impl Into<String>, seed: u64, settings: ScenarioSettings) -> Self {
        Self {
            format: SCENARIO_FORMAT.to_string(),
            version: SCENARIO_VERSION,
            name: name.into(),
            build: build_id(),
            recorded_at: None,
            seed,
            settings,
            controls: Vec::new(),
            actions: Vec::new(),
            captures: Vec::new(),
            overlays: Overlays::default(),
            exit_tick: 0,
            expect: None,
        }
    }

    /// Parses and validates RON text.
    pub fn from_ron_str(text: &str) -> Result<Self, ScenarioError> {
        let scenario: Self =
            ron::from_str(text).map_err(|e| ScenarioError::Parse(e.to_string()))?;
        scenario.validate()?;
        Ok(scenario)
    }

    /// Reads, parses and validates a scenario file.
    pub fn load(path: &Path) -> Result<Self, ScenarioError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ScenarioError::Io(format!("{}: {e}", path.display())))?;
        Self::from_ron_str(&text)
    }

    /// Pretty RON text for this scenario.
    pub fn to_ron_string(&self) -> Result<String, ScenarioError> {
        let config = ron::ser::PrettyConfig::new()
            .depth_limit(3)
            .separate_tuple_members(false)
            .enumerate_arrays(false);
        ron::ser::to_string_pretty(self, config).map_err(|e| ScenarioError::Parse(e.to_string()))
    }

    /// Writes this scenario to `path`, creating parent directories.
    pub fn save(&self, path: &Path) -> Result<(), ScenarioError> {
        let text = self.to_ron_string()?;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| ScenarioError::Io(format!("{}: {e}", parent.display())))?;
        }
        std::fs::write(path, text).map_err(|e| ScenarioError::Io(format!("{}: {e}", path.display())))
    }

    /// Checks the format tag, version and every input stream; never panics on hostile data.
    pub fn validate(&self) -> Result<(), ScenarioError> {
        if self.format != SCENARIO_FORMAT {
            return Err(ScenarioError::Format(format!("format tag is {:?}", self.format)));
        }
        if self.version != SCENARIO_VERSION {
            return Err(ScenarioError::Format(format!(
                "unsupported version {} (expected {SCENARIO_VERSION})",
                self.version
            )));
        }
        if self.exit_tick > MAX_SCENARIO_TICKS {
            return Err(ScenarioError::Invalid(format!(
                "exit_tick {} exceeds {MAX_SCENARIO_TICKS}",
                self.exit_tick
            )));
        }
        let mut prev: Option<u64> = None;
        for entry in &self.controls {
            if prev.is_some_and(|p| entry.0 <= p) {
                return Err(ScenarioError::Invalid(format!(
                    "control ticks must strictly increase (tick {})",
                    entry.0
                )));
            }
            if !(entry.1.is_finite() && entry.2.is_finite())
                || entry.1.abs() > 2.0
                || entry.2.abs() > 2.0
            {
                return Err(ScenarioError::Invalid(format!(
                    "control at tick {} must be finite and within -2.0..=2.0",
                    entry.0
                )));
            }
            prev = Some(entry.0);
        }
        if self.actions.windows(2).any(|w| matches!(w, [a, b] if b.at() < a.at())) {
            return Err(ScenarioError::Invalid("actions must be sorted by tick".to_string()));
        }
        if self.captures.windows(2).any(|w| matches!(w, [a, b] if b.tick < a.tick)) {
            return Err(ScenarioError::Invalid("captures must be sorted by tick".to_string()));
        }
        if let Some(bad) = self.captures.iter().find(|c| c.tick > self.exit_tick) {
            return Err(ScenarioError::Invalid(format!(
                "capture {:?} at tick {} is after exit_tick {}",
                bad.label, bad.tick, self.exit_tick
            )));
        }
        let u = self.settings.upgrades;
        if !(u.ar400_edge.is_finite() && u.charge_air_kit.is_finite() && u.aux_ram.is_finite()) {
            return Err(ScenarioError::Invalid("upgrades must be finite".to_string()));
        }
        Ok(())
    }

    /// Control in effect at `tick` (idle before the first entry).
    #[must_use]
    pub fn control_at(&self, tick: u64) -> DriveControl {
        let idx = self.controls.partition_point(|c| c.0 <= tick);
        idx.checked_sub(1)
            .and_then(|i| self.controls.get(i))
            .map_or_else(DriveControl::default, ControlEntry::control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> ScenarioSettings {
        ScenarioSettings {
            mode: GameMode::Sandbox,
            level: CampaignLevel::County1,
            season: Season::Summer,
            weather: WeatherSetting::Off,
            test_map: SiteTestMap::Standard,
            upgrades: UpgradeMultipliers::default(),
        }
    }

    #[test]
    fn roundtrip_preserves_every_float_bit() -> Result<(), ScenarioError> {
        let mut s = Scenario::new("rt", 42, settings());
        s.controls = vec![
            ControlEntry(0, 1.0, -1.0, false),
            ControlEntry(17, 0.1 + 0.2, f32::MIN_POSITIVE, true),
        ];
        s.actions = vec![ScenarioAction::Upgrade {
            at: 5,
            choice: UpgradeChoice::AuxRam,
        }];
        s.captures = vec![CaptureSpec {
            tick: 9,
            label: "mid".into(),
        }];
        s.exit_tick = 30;
        let back = Scenario::from_ron_str(&s.to_ron_string()?)?;
        assert_eq!(back, s);
        let bits = |c: &ControlEntry| (c.1.to_bits(), c.2.to_bits());
        assert_eq!(back.controls.iter().map(bits).collect::<Vec<_>>(), s.controls.iter().map(bits).collect::<Vec<_>>());
        Ok(())
    }

    #[test]
    fn hostile_inputs_are_rejected_not_panicked() {
        let mut s = Scenario::new("bad", 1, settings());
        s.exit_tick = 10;
        s.controls = vec![ControlEntry(3, f32::NAN, 0.0, false)];
        assert!(matches!(s.validate(), Err(ScenarioError::Invalid(_))));
        s.controls = vec![ControlEntry(3, 0.0, 0.0, false), ControlEntry(3, 1.0, 1.0, false)];
        assert!(matches!(s.validate(), Err(ScenarioError::Invalid(_))));
        s.controls.clear();
        s.exit_tick = u64::MAX;
        assert!(matches!(s.validate(), Err(ScenarioError::Invalid(_))));
        s.exit_tick = 10;
        s.captures = vec![CaptureSpec { tick: 11, label: "late".into() }];
        assert!(matches!(s.validate(), Err(ScenarioError::Invalid(_))));
        s.captures.clear();
        s.format = "something-else".into();
        assert!(matches!(s.validate(), Err(ScenarioError::Format(_))));
        assert!(matches!(Scenario::from_ron_str("not ron at all ("), Err(ScenarioError::Parse(_))));
    }

    #[test]
    fn control_at_holds_last_entry_and_idles_before_first() {
        let mut s = Scenario::new("ctl", 1, settings());
        s.controls = vec![ControlEntry(4, 1.0, 1.0, true), ControlEntry(10, -1.0, 1.0, false)];
        assert_eq!(s.control_at(0), DriveControl::default());
        assert!(s.control_at(4).blade);
        assert!(s.control_at(9).blade);
        assert!((s.control_at(10).left + 1.0).abs() < f32::EPSILON);
        assert!((s.control_at(u64::MAX).left + 1.0).abs() < f32::EPSILON);
    }
}
