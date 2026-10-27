//! Text harness for privileged arithmetic/topology verification. Not a battle API.
use quark_drive_core::math::{self, DamageInput, Modifier, NatureEffect, StatRule};
use quark_drive_core::position::{valid_target, ActiveSlot, Side, TargetKind, TargetLoc};
use quark_drive_core::state::State;
use std::error::Error;
use std::io::{self, BufRead, Write};
use std::mem::size_of;

fn boolean(text: &str) -> Result<bool, Box<dyn Error>> {
    match text { "0" => Ok(false), "1" => Ok(true), _ => Err("invalid boolean".into()) }
}

fn target<const ACTIVE: usize>(source: u8, loc: i8, kind: TargetKind) -> bool {
    match TargetLoc::new::<ACTIVE>(loc) {
        Ok(location) => valid_target::<ACTIVE>(ActiveSlot {side: Side::One, index: source}, location, kind),
        Err(_) => false,
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut output = io::BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let line = line?;
        let fields: Vec<_> = line.split('\t').collect();
        match fields.first().copied() {
            Some("S") if fields.len() == 7 => {
                let rule = match fields[5].parse::<u8>()? {
                    0 => StatRule::ChampionsFlat,
                    level => StatRule::ChampionsScaled {level},
                };
                let nature = match fields[4] {"0" => NatureEffect::Neutral, "1" => NatureEffect::Raised,
                    "-1" => NatureEffect::Lowered, _ => return Err("invalid nature".into())};
                let value = math::stat(rule, fields[1].parse()?, fields[2].parse()?, boolean(fields[3])?, nature, boolean(fields[6])?)
                    .map_err(|_| "stat rejected")?;
                writeln!(output, "{value}")?;
            }
            Some("D") if fields.len() == 14 => {
                let modifier = |text: &str| -> Result<Modifier, Box<dyn Error>> {
                    Modifier::from_fixed(text.parse()?).map_err(|_| "modifier rejected".into())
                };
                let value = math::damage(DamageInput {
                    level: fields[1].parse()?, power: fields[2].parse()?, attack: fields[3].parse()?, defense: fields[4].parse()?,
                    spread: boolean(fields[5])?, weather: modifier(fields[6])?, critical: boolean(fields[7])?, roll: fields[8].parse()?,
                    stab: modifier(fields[9])?, effectiveness: fields[10].parse()?, burn_halves_damage: boolean(fields[11])?,
                    final_modifier: modifier(fields[12])?, quarter_protect: boolean(fields[13])?,
                }).map_err(|_| "damage rejected")?;
                writeln!(output, "{value}")?;
            }
            Some("T") if fields.len() == 5 => {
                let kind = match fields[4].parse::<u8>()? {0 => TargetKind::Normal, 1 => TargetKind::RandomNormal,
                    2 => TargetKind::Scripted, 3 => TargetKind::AdjacentAlly, 4 => TargetKind::AdjacentAllyOrSelf,
                    5 => TargetKind::AdjacentFoe, 6 => TargetKind::Any, 7 => TargetKind::Automatic,
                    _ => return Err("invalid target kind".into())};
                let source = fields[2].parse()?;
                let location = fields[3].parse()?;
                let valid = match fields[1] {"1" => target::<1>(source, location, kind), "2" => target::<2>(source, location, kind),
                    _ => return Err("invalid format".into())};
                writeln!(output, "{}", u8::from(valid))?;
            }
            Some("M") if fields.len() == 1 => writeln!(output, "{}\t{}", size_of::<State<1>>(), size_of::<State<2>>())?,
            _ => return Err("invalid core probe command".into()),
        }
    }
    Ok(())
}
