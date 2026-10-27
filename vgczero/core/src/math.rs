//! Scalar battle arithmetic. Callers must resolve event hooks, immunity, selected stats,
//! boosts and modifiers before using this module; it does not claim full move support.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArithmeticError {
    InvalidStat,
    InvalidDamage,
    InvalidModifier,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NatureEffect {
    Neutral,
    Raised,
    Lowered,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatRule {
    ChampionsFlat,
    ChampionsScaled { level: u8 },
}

/// The pinned Champions statModify override. The base-HP=1 species exception belongs
/// to spreadModify, not this per-stat function. SP uses the Champions 0..32 domain.
pub fn stat(
    rule: StatRule,
    base: u16,
    sp: u8,
    hp: bool,
    nature: NatureEffect,
    overflow_stat_mod: bool,
) -> Result<u16, ArithmeticError> {
    if base == 0 || base > 255 || sp > 32 {
        return Err(ArithmeticError::InvalidStat);
    }
    let base = u32::from(base);
    let sp = u32::from(sp);
    let value = match rule {
        StatRule::ChampionsFlat => base + sp + if hp { 75 } else { 20 },
        StatRule::ChampionsScaled { level } => {
            if !(1..=100).contains(&level) {
                return Err(ArithmeticError::InvalidStat);
            }
            (2 * base + 31 + (2 * sp).saturating_sub(1)) * u32::from(level) / 100
                + if hp { u32::from(level) + 10 } else { 5 }
        }
    };
    let adjusted = if hp {
        value
    } else {
        match nature {
            NatureEffect::Neutral => value,
            NatureEffect::Raised => {
                let value = if overflow_stat_mod { value.min(595) } else { value };
                ((value * 110) & 0xffff) / 100
            }
            NatureEffect::Lowered => {
                let value = if overflow_stat_mod { value.min(728) } else { value };
                ((value * 90) & 0xffff) / 100
            }
        }
    };
    Ok(adjusted as u16)
}

/// Gen9 stage arithmetic, before ability/item/field hooks. Critical-hit stage bypass
/// is resolved by the caller rather than guessed here.
pub fn stage(value: u16, boost: i8) -> Result<u16, ArithmeticError> {
    if value == 0 || !(-6..=6).contains(&boost) {
        return Err(ArithmeticError::InvalidStat);
    }
    let result = if boost >= 0 {
        u32::from(value) * (2 + boost as u32) / 2
    } else {
        u32::from(value) * 2 / (2 + u32::from(boost.unsigned_abs()))
    };
    u16::try_from(result).map_err(|_| ArithmeticError::InvalidStat)
}

/// Fixed-point modifier with Showdown's 4096 denominator and half-down rounding.
/// The upper bound covers the scalar modifiers accepted by this initial primitive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Modifier(u16);

impl Modifier {
    pub const ONE: Self = Self(4096);
    pub const HALF: Self = Self(2048);
    pub const THREE_QUARTERS: Self = Self(3072);
    pub const THREE_HALVES: Self = Self(6144);

    pub fn from_fixed(value: u16) -> Result<Self, ArithmeticError> {
        if value == 0 || value > 16384 {
            return Err(ArithmeticError::InvalidModifier);
        }
        Ok(Self(value))
    }

    pub fn apply(self, value: u32) -> u32 {
        ((u64::from(value.wrapping_mul(u32::from(self.0))) + 2047) / 4096) as u32
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DamageInput {
    pub level: u8,
    pub power: u32,
    pub attack: u16,
    pub defense: u16,
    pub spread: bool,
    pub weather: Modifier,
    pub critical: bool,
    /// Actual randomizer value: 100 - random(16), hence 85..=100.
    pub roll: u8,
    pub stab: Modifier,
    /// Immunity must be resolved before this primitive is called.
    pub effectiveness: i8,
    pub burn_halves_damage: bool,
    pub final_modifier: Modifier,
    pub quarter_protect: bool,
}

pub fn damage(input: DamageInput) -> Result<u16, ArithmeticError> {
    if !(1..=100).contains(&input.level)
        || input.power == 0
        || input.power > 1_000_000
        || input.attack == 0
        || input.defense == 0
        || !(85..=100).contains(&input.roll)
        || !(-6..=6).contains(&input.effectiveness)
    {
        return Err(ArithmeticError::InvalidDamage);
    }
    let level = 2 * u32::from(input.level) / 5 + 2;
    let mut value = level
        .wrapping_mul(input.power)
        .wrapping_mul(u32::from(input.attack))
        / u32::from(input.defense)
        / 50;
    value += 2;
    if input.spread {
        value = Modifier::THREE_QUARTERS.apply(value);
    }
    value = input.weather.apply(value);
    if input.critical {
        value = value.wrapping_mul(3) / 2;
    }
    value = value.wrapping_mul(u32::from(input.roll)) / 100;
    value = input.stab.apply(value);
    if input.effectiveness >= 0 {
        value = value.wrapping_shl(input.effectiveness as u32);
    } else {
        value >>= input.effectiveness.unsigned_abs();
    }
    if input.burn_halves_damage {
        value = Modifier::HALF.apply(value);
    }
    value = input.final_modifier.apply(value);
    if input.quarter_protect {
        value = Modifier(1024).apply(value);
    }
    // The minimum occurs before the final 16-bit truncation in gen9 Showdown.
    Ok(if value == 0 { 1 } else { value as u16 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point_half_down_and_parameter_failures() {
        assert_eq!(Modifier::HALF.apply(3), 1);
        assert_eq!(Modifier::THREE_HALVES.apply(3), 4);
        assert_eq!(stat(StatRule::ChampionsFlat, 100, 32, false, NatureEffect::Raised, false), Ok(167));
        assert_eq!(stat(StatRule::ChampionsFlat, 100, 33, false, NatureEffect::Raised, false), Err(ArithmeticError::InvalidStat));
        assert_eq!(stage(167, -1), Ok(111));
        assert_eq!(stage(100, 7), Err(ArithmeticError::InvalidStat));
        assert_eq!(Modifier::from_fixed(0), Err(ArithmeticError::InvalidModifier));
    }
}
