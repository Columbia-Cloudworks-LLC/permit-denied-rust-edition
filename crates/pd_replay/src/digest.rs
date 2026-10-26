//! End-of-run digest compared after a replay (`MATCH` / `DRIFT`), mirroring the reference's
//! `PlaythroughDigest` (cash, elapsed, dozer pose, heat, track, rubble, cell states, cell HP)
//! plus a bit-exact `state_hash` over every debris chunk, prop and structural cell.

use bevy::prelude::*;
use pd_core::{CashLedger, ElapsedClock, SimOrdinal, SimTick, SplitMix64, WorldPose};
use pd_destruction::{BuildingGraphRegistry, DamageState, DebrisChunk, DestructibleProp};
use pd_dozer::{MechanicalStress, PlayerVehicle, UpgradeMultipliers};
use pd_world::PileField;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Dozer pose and gauges (`ENGINE HEAT`, `TRACK STRESS`).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct DozerDigest {
    pub x: f32,
    pub y: f32,
    pub heading: f32,
    pub heat: f32,
    pub track: f32,
}

/// Simulation state a replay must reproduce bit-for-bit on the same build and OS.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Digest {
    pub ticks: u64,
    pub cash: u32,
    pub elapsed: f32,
    pub dozer: DozerDigest,
    pub upgrades: UpgradeMultipliers,
    /// Debris chunks on the site (active, sleeping and crushed tiers).
    pub debris: u32,
    pub debris_mass: f32,
    pub pile_mass: f32,
    /// Structural cells by state: `intact`, `cracked`, `destroyed`.
    pub cells: BTreeMap<String, u32>,
    pub cell_hp: f32,
    /// Bit-exact fold over every cell, debris chunk and prop in stable order.
    pub state_hash: u64,
}

fn fold(acc: u64, value: u64) -> u64 {
    SplitMix64::mix(acc ^ value)
}

fn fold_f32(acc: u64, value: f32) -> u64 {
    fold(acc, u64::from(value.to_bits()))
}

/// Computes the digest of the current world. Visits buildings by `site_ordinal`, cells by
/// node id and debris/props by `SimOrdinal`, so the result is independent of query order.
pub fn compute_digest(world: &mut World) -> Digest {
    let mut digest = Digest {
        ticks: world.get_resource::<SimTick>().map_or(0, |t| t.0),
        cash: world.get_resource::<CashLedger>().map_or(0, |l| l.current_cash),
        elapsed: world.get_resource::<ElapsedClock>().map_or(0.0, |c| c.elapsed_secs),
        upgrades: world.get_resource::<UpgradeMultipliers>().copied().unwrap_or_default(),
        pile_mass: world.get_resource::<PileField>().map_or(0.0, PileField::total_mass),
        ..Digest::default()
    };
    if let Some(stress) = world.get_resource::<MechanicalStress>() {
        digest.dozer.heat = stress.engine_heat_pct;
        digest.dozer.track = stress.track_stress_pct;
    }
    let mut dozer = world.query_filtered::<&WorldPose, With<PlayerVehicle>>();
    if let Some(pose) = dozer.iter(world).next() {
        digest.dozer.x = pose.pos.x;
        digest.dozer.y = pose.pos.y;
        digest.dozer.heading = pose.heading_rad;
    }

    let mut hash = 0_u64;
    let (mut intact, mut cracked, mut destroyed) = (0_u32, 0_u32, 0_u32);
    let mut cell_hp = 0.0_f32;
    if let Some(registry) = world.get_resource::<BuildingGraphRegistry>() {
        let mut buildings: Vec<_> = registry.buildings.values().collect();
        buildings.sort_by_key(|b| b.site_ordinal);
        for building in buildings {
            let mut ids: Vec<_> = building.nodes.keys().copied().collect();
            ids.sort_unstable();
            for id in ids {
                let Some(node) = building.nodes.get(&id) else {
                    continue;
                };
                match (node.intact, node.damage_state) {
                    (false, _) => destroyed = destroyed.saturating_add(1),
                    (true, DamageState::CrackedStressed) => cracked = cracked.saturating_add(1),
                    (true, _) => intact = intact.saturating_add(1),
                }
                cell_hp += node.hp;
                hash = fold(fold_f32(fold_f32(hash, node.hp), node.cladding_hp), u64::from(node.intact));
            }
        }
    }
    digest.cells = BTreeMap::from([
        ("intact".to_string(), intact),
        ("cracked".to_string(), cracked),
        ("destroyed".to_string(), destroyed),
    ]);
    digest.cell_hp = cell_hp;

    let mut debris = world.query::<(&SimOrdinal, &DebrisChunk, &WorldPose)>();
    let mut rows: Vec<_> = debris.iter(world).map(|(o, c, p)| (o.0, *c, *p)).collect();
    rows.sort_unstable_by_key(|r| r.0);
    digest.debris = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    for (ordinal, chunk, pose) in &rows {
        digest.debris_mass += chunk.mass;
        hash = fold(hash, *ordinal);
        hash = fold(hash, chunk.tier as u64);
        hash = fold_f32(fold_f32(fold_f32(hash, pose.pos.x), pose.pos.y), pose.elev_z);
        hash = fold_f32(hash, chunk.mass);
    }

    let mut props = world.query::<(&SimOrdinal, &DestructibleProp)>();
    let mut prop_rows: Vec<_> = props.iter(world).map(|(o, p)| (o.0, p.hp)).collect();
    prop_rows.sort_unstable_by_key(|r| r.0);
    for (ordinal, hp) in prop_rows {
        hash = fold_f32(fold(hash, ordinal), hp);
    }
    digest.state_hash = hash;
    digest
}

/// Flattens a digest into `(field, exact bits)` pairs for comparison and reporting.
fn flatten(d: &Digest) -> Vec<(String, u64)> {
    let f = |v: f32| u64::from(v.to_bits());
    let mut out = vec![
        ("ticks".to_string(), d.ticks),
        ("cash".to_string(), u64::from(d.cash)),
        ("elapsed".to_string(), f(d.elapsed)),
        ("dozer.x".to_string(), f(d.dozer.x)),
        ("dozer.y".to_string(), f(d.dozer.y)),
        ("dozer.heading".to_string(), f(d.dozer.heading)),
        ("dozer.heat".to_string(), f(d.dozer.heat)),
        ("dozer.track".to_string(), f(d.dozer.track)),
        ("upgrades.ar400_edge".to_string(), f(d.upgrades.ar400_edge)),
        ("upgrades.charge_air_kit".to_string(), f(d.upgrades.charge_air_kit)),
        ("upgrades.aux_ram".to_string(), f(d.upgrades.aux_ram)),
        ("debris".to_string(), u64::from(d.debris)),
        ("debris_mass".to_string(), f(d.debris_mass)),
        ("pile_mass".to_string(), f(d.pile_mass)),
        ("cell_hp".to_string(), f(d.cell_hp)),
        ("state_hash".to_string(), d.state_hash),
    ];
    out.extend(d.cells.iter().map(|(k, v)| (format!("cells.{k}"), u64::from(*v))));
    out
}

/// Field names whose values differ (bit-exact); empty when the replay matched.
#[must_use]
pub fn digest_drift(expected: &Digest, actual: &Digest) -> Vec<String> {
    let a: BTreeMap<String, u64> = flatten(expected).into_iter().collect();
    let b: BTreeMap<String, u64> = flatten(actual).into_iter().collect();
    let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drift_is_bit_exact_and_names_fields() {
        let base = Digest {
            ticks: 10,
            cash: 5,
            elapsed: 0.5,
            cells: BTreeMap::from([("intact".to_string(), 3)]),
            ..Digest::default()
        };
        assert!(digest_drift(&base, &base.clone()).is_empty());

        let mut nudged = base.clone();
        nudged.dozer.x = f32::from_bits(base.dozer.x.to_bits() + 1);
        nudged.cells.insert("destroyed".to_string(), 1);
        assert_eq!(digest_drift(&base, &nudged), vec!["cells.destroyed", "dozer.x"]);

        // -0.0 and 0.0 compare equal as floats but must count as drift.
        let mut signed = base.clone();
        signed.elapsed = -0.0;
        let mut zero = base;
        zero.elapsed = 0.0;
        assert_eq!(digest_drift(&zero, &signed), vec!["elapsed"]);
    }
}
