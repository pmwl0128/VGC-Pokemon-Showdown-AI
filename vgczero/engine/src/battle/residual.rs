//! End-of-turn effects in Showdown's residual order: every handler from the
//! field, sides and Pokemon is sorted by (order, speed desc, subOrder), and
//! durations tick at their handler's slot.

use super::effects::{BoostSrc, DmgKind};
use super::ties::{Ev, Hk, Whitelist};
use super::{blog, Battle};
use crate::dex::{Cond, Status, Terrain, Weather, B_SPE};
use crate::kinds::{Ab, It};
use crate::state::{Pos, ProtectKind};
use crate::types::Type;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Res {
    Weather,
    Wish,
    GrassyHeal,
    Healer,
    Leftovers,
    AquaRing,
    Ingrain,
    LeechSeed,
    Poison,
    Burn,
    Curse,
    SaltCure,
    PartialTrap,
    Taunt,
    Encore,
    Disable,
    MagnetRise,
    HealBlock,
    ThroatChop,
    Yawn,
    PerishSong,
    TrickRoom,
    Gravity,
    Terrain,
    SpeedBoost,
    Moody,
    Roost,
    WhiteHerb,
    // Duration-only volatiles: they only count down (order last, subOrder 2).
    Flinch,
    Protect,
    Endure,
    Stall,
    HelpingHand,
    FollowMe,
    TwoTurnMove,
    TwoTurnSpent,
    MustRecharge,
    // Side conditions (subOrder from the move data).
    Reflect,
    LightScreen,
    Safeguard,
    Mist,
    Tailwind,
    AuroraVeil,
    WideGuard,
    QuickGuard,
}

/// Who holds a residual handler: a Pokemon (position code), a side or the field.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Holder {
    Mon(u8),
    Side(u8),
    Field,
}

/// One fieldEvent('Residual') handler with Showdown's sort keys (order: u32::MAX for none)
/// and its effect's rank (id order, ties.rs).
#[derive(Copy, Clone, Debug)]
struct Rh {
    order: u32,
    speed: i32,
    sub: u8,
    holder: Holder,
    id: u16,
    kind: Res,
    slot: bool,
}

const LAST: u32 = u32::MAX;

impl Battle {
    pub(crate) fn run_residual(&mut self) {
        crate::prof!("run_residual");
        // Revival Blessing's slot condition (duration 1) ends.
        for s in 0..2 {
            self.sides[s].reviving = [false; 2];
        }
        // HP before residuals, for Emergency Exit.
        let mut hp_before = [0u16; 4];
        for c in 0..4 {
            let p = crate::state::Pos::from_code(c);
            if self.is_live(p) {
                hp_before[c as usize] = self.m(p).hp;
            }
        }
        let items = self.residual_handlers();
        // speedSort (whitelisted artificial ties: one Pokemon, or only field / sides, keep the
        // patched oracle's fixed order by effect id and holder); real ties are shuffled.
        let mut keyed = items;
        let wl = &self.dex.whitelist;
        crate::rng::speed_sort_with(
            &mut self.rng,
            &mut keyed,
            |a, b| a.order.cmp(&b.order).then(b.speed.cmp(&a.speed)).then(a.sub.cmp(&b.sub)),
            |g| {
                // Determinized only when artificial (one holder, or all speedless of one kind)
                // and in a whitelisted category.
                let kind = holder_kind(g[0].holder);
                let one_holder = g.iter().all(|h| h.holder == g[0].holder);
                let speedless = g.iter().all(|h| !matches!(h.holder, Holder::Mon(_)) && holder_kind(h.holder) == kind);
                if !(one_holder || speedless) {
                    return false;
                }
                if g.len() > Whitelist::MAX_EFFECTS {
                    return false;
                }
                let mut ids = [0u16; Whitelist::MAX_EFFECTS];
                for (k, h) in g.iter().enumerate() {
                    ids[k] = h.id;
                }
                wl.contains(Ev::Residual, &ids[..g.len()], kind)
            },
            |a, b| a.id.cmp(&b.id).then(holder_key(a.holder).cmp(&holder_key(b.holder))),
        );
        for &h in &keyed {
            if self.ended() {
                return;
            }
            let ended = self.residual_ends(&h);
            match h.holder {
                Holder::Mon(code) => {
                    let p = Pos::from_code(code);
                    // A fainted holder's handlers are skipped (slot conditions still run); one
                    // at 0 HP that has not fainted yet still runs them.
                    if !h.slot && !self.active_unfainted(p) {
                        continue;
                    }
                    if h.slot {
                        self.residual_field(h.kind, code);
                    } else {
                        self.residual_mon(h.kind, p);
                    }
                }
                Holder::Side(s) => self.residual_field(h.kind, s),
                Holder::Field => self.residual_field(h.kind, 255),
            }
            // A handler whose duration ran out calls its end and `continue`s: fieldEvent runs
            // no faintMessages after it (simultaneous perishes faint together).
            if ended {
                continue;
            }
            self.check_win_quiet();
        }
        // Per-turn counters.
        for c in 0..4 {
            let p = Pos::from_code(c);
            if self.is_live(p) {
                let m = self.mm(p);
                m.vol.active_turns = m.vol.active_turns.saturating_add(1);
            }
        }
        // Per-turn flags outside Showdown's volatile set.
        for c in 0..4 {
            let p = Pos::from_code(c);
            if self.sides[p.s()].active[p.i()] != crate::state::NO_MON {
                let v = &mut self.mm(p).vol;
                v.destiny_bond_pending = false;
                v.laser_focus = v.laser_focus.saturating_sub(1);
            }
        }
        let _ = hp_before;
    }

    /// fieldEvent('Residual')'s handlers in Showdown's collection order: field
    /// (pseudo-weather, weather, terrain), each side's conditions, then for each active
    /// Pokemon (fainted ones too) its status, volatiles, ability, item, slot conditions and
    /// the field's per-Pokemon handlers (Grassy Terrain). Keys from the pinned data.
    fn residual_handlers(&self) -> crate::fixed::FixedVec<Rh, 64> {
        let d = self.dex;
        let mut v: crate::fixed::FixedVec<Rh, 64> = crate::fixed::FixedVec::new();
        let field = |order: u32, sub: u8, id: Cond, kind: Res| Rh { order, speed: 0, sub, holder: Holder::Field, id: d.cr(id), kind, slot: false };
        if self.field.trick_room > 0 {
            v.push(field(27, 1, Cond::TrickRoom, Res::TrickRoom));
        }
        if self.field.gravity > 0 {
            v.push(field(27, 2, Cond::Gravity, Res::Gravity));
        }
        if self.field.weather != Weather::None {
            v.push(field(1, 5, Cond::Weather, Res::Weather));
        }
        if self.field.terrain != Terrain::None {
            v.push(field(27, 7, Cond::Terrain, Res::Terrain));
        }
        for s in 0..2u8 {
            let c = &self.sides[s as usize].conds;
            let side = |order: u32, sub: u8, id: Cond, kind: Res| Rh { order, speed: 0, sub, holder: Holder::Side(s), id: d.cr(id), kind, slot: false };
            if c.reflect > 0 {
                v.push(side(26, 1, Cond::Reflect, Res::Reflect));
            }
            if c.light_screen > 0 {
                v.push(side(26, 2, Cond::LightScreen, Res::LightScreen));
            }
            if c.safeguard > 0 {
                v.push(side(26, 3, Cond::Safeguard, Res::Safeguard));
            }
            if c.mist > 0 {
                v.push(side(26, 4, Cond::Mist, Res::Mist));
            }
            if c.tailwind > 0 {
                v.push(side(26, 5, Cond::Tailwind, Res::Tailwind));
            }
            if c.aurora_veil > 0 {
                v.push(side(26, 10, Cond::AuroraVeil, Res::AuroraVeil));
            }
            if c.wide_guard {
                v.push(side(LAST, 4, Cond::WideGuard, Res::WideGuard));
            }
            if c.quick_guard {
                v.push(side(LAST, 4, Cond::QuickGuard, Res::QuickGuard));
            }
        }
        for c in 0..4u8 {
            let p = Pos::from_code(c);
            let i = self.sides[p.s()].active[p.i()];
            if i == crate::state::NO_MON {
                continue;
            }
            let m = &self.sides[p.s()].mons[i as usize];
            let spd = m.speed;
            let mon = |order: u32, sub: u8, id: Cond, kind: Res| Rh { order, speed: spd, sub, holder: Holder::Mon(c), id: d.cr(id), kind, slot: false };
            // Status (effectType Status: subOrder 0). A Pokemon that fainted this turn keeps
            // its status until checkFainted.
            let st = if m.fainted { m.status_before_faint } else { m.status };
            match st {
                Status::Psn => v.push(mon(9, 0, Cond::Psn, Res::Poison)),
                Status::Tox => v.push(mon(9, 0, Cond::Tox, Res::Poison)),
                Status::Brn => v.push(mon(10, 0, Cond::Brn, Res::Burn)),
                _ => {}
            }
            // Volatiles (Condition: subOrder 2).
            let w = m.vol;
            if !m.fainted {
                if w.aqua_ring {
                    v.push(mon(6, 2, Cond::AquaRing, Res::AquaRing));
                }
                if w.ingrain {
                    v.push(mon(7, 2, Cond::Ingrain, Res::Ingrain));
                }
                if w.leech_seed {
                    v.push(mon(8, 2, Cond::LeechSeed, Res::LeechSeed));
                }
                if w.curse {
                    v.push(mon(12, 2, Cond::Curse, Res::Curse));
                }
                if w.salt_cure {
                    v.push(mon(13, 2, Cond::SaltCure, Res::SaltCure));
                }
                if w.partial_trap > 0 {
                    v.push(mon(13, 2, Cond::PartiallyTrapped, Res::PartialTrap));
                }
                if w.taunt > 0 {
                    v.push(mon(15, 2, Cond::Taunt, Res::Taunt));
                }
                if w.encore > 0 {
                    v.push(mon(16, 2, Cond::Encore, Res::Encore));
                }
                if w.disable > 0 {
                    v.push(mon(17, 2, Cond::Disable, Res::Disable));
                }
                if w.magnet_rise > 0 {
                    v.push(mon(18, 2, Cond::MagnetRise, Res::MagnetRise));
                }
                if w.heal_block > 0 {
                    v.push(mon(20, 2, Cond::HealBlock, Res::HealBlock));
                }
                if w.throat_chop > 0 {
                    v.push(mon(22, 2, Cond::ThroatChop, Res::ThroatChop));
                }
                if w.yawn > 0 {
                    v.push(mon(23, 2, Cond::Yawn, Res::Yawn));
                }
                if w.perish > 0 {
                    v.push(mon(24, 2, Cond::PerishSong, Res::PerishSong));
                }
                if w.roost {
                    v.push(mon(25, 2, Cond::Roost, Res::Roost));
                }
                if w.flinch {
                    v.push(mon(LAST, 2, Cond::Flinch, Res::Flinch));
                }
                let protect_id = match w.protect {
                    ProtectKind::None => None,
                    ProtectKind::Protect => Some(Cond::Protect),
                    ProtectKind::SpikyShield => Some(Cond::SpikyShield),
                    ProtectKind::BanefulBunker => Some(Cond::BanefulBunker),
                    ProtectKind::KingsShield => Some(Cond::KingsShield),
                    ProtectKind::SilkTrap => Some(Cond::SilkTrap),
                    ProtectKind::BurningBulwark => Some(Cond::BurningBulwark),
                };
                if let Some(id) = protect_id {
                    v.push(mon(LAST, 2, id, Res::Protect));
                }
                if w.endure {
                    v.push(mon(LAST, 2, Cond::Endure, Res::Endure));
                }
                if w.stall_dur > 0 {
                    v.push(mon(LAST, 2, Cond::Stall, Res::Stall));
                }
                if w.helping_hand > 0 {
                    v.push(mon(LAST, 2, Cond::HelpingHand, Res::HelpingHand));
                }
                match w.follow_me {
                    1 => v.push(mon(LAST, 2, Cond::FollowMe, Res::FollowMe)),
                    2 => v.push(mon(LAST, 2, Cond::RagePowder, Res::FollowMe)),
                    _ => {}
                }
                if w.twoturn_spent {
                    v.push(mon(LAST, 2, Cond::TwoTurnMove, Res::TwoTurnSpent));
                }
                if w.charging > 0 {
                    v.push(mon(LAST, 2, Cond::TwoTurnMove, Res::TwoTurnMove));
                    // Semi-invulnerable charges also hold the move's own 2-turn condition.
                    let cm = d.mv(m.moves[(w.charging - 1) as usize]);
                    if matches!(cm.id.as_str(), "fly" | "bounce" | "dig" | "dive" | "phantomforce") {
                        v.push(Rh { order: LAST, speed: spd, sub: 2, holder: Holder::Mon(c), id: cm.rank, kind: Res::TwoTurnMove, slot: false });
                    }
                }
                if w.must_recharge {
                    v.push(mon(LAST, 2, Cond::MustRecharge, Res::MustRecharge));
                }
            }
            // Ability (subOrder 7 unless given), item (8 unless given).
            let ab = self.dex.ab(m.ability);
            match ab {
                Ab::Healer => v.push(mon(5, 3, Cond::Healer, Res::Healer)),
                Ab::SpeedBoost => v.push(mon(28, 2, Cond::SpeedBoost, Res::SpeedBoost)),
                Ab::Moody => v.push(mon(28, 2, Cond::Moody, Res::Moody)),
                _ => {}
            }
            match self.dex.it(m.item) {
                It::Leftovers => v.push(mon(5, 4, Cond::Leftovers, Res::Leftovers)),
                It::WhiteHerb => v.push(mon(29, 8, Cond::WhiteHerb, Res::WhiteHerb)),
                _ => {}
            }
            // Slot condition: Wish (slot conditions run even for a fainted holder).
            if self.sides[p.s()].wish[p.i()].0 > 0 {
                v.push(Rh { order: 4, speed: spd, sub: 3, holder: Holder::Mon(c), id: d.cr(Cond::Wish), kind: Res::Wish, slot: true });
            }
            // The field's per-Pokemon handler: Grassy Terrain heals (order 5, subOrder 2).
            if self.field.terrain == Terrain::Grassy {
                v.push(mon(5, 2, Cond::GrassyTerrain, Res::GrassyHeal));
            }
        }
        v
    }

    fn check_win_quiet(&mut self) {
        // Faint Pokemon at 0 HP as residuals go (Showdown faints after each handler).
        self.process_faints();
    }

    fn residual_field(&mut self, kind: Res, code: u8) {
        match kind {
            Res::Weather => {
                if self.field.weather == Weather::None {
                    return;
                }
                self.field.weather_turns = self.field.weather_turns.saturating_sub(1);
                if self.field.weather_turns == 0 {
                    self.clear_weather();
                    return;
                }
                let w = self.weather();
                // Weather effects on each active Pokemon, by speed.
                // eachEvent('Weather') then eachEvent('Update'); Pokemon faint
                // only after the whole handler (fieldEvent's faintMessages).
                let (ps, n) = self.each_event_order();
                for &p in &ps[..n] {
                    if !self.is_live(p) {
                        continue;
                    }
                    self.weather_effect(p, w);
                }
                self.each_event_update();
                self.process_faints();
            }
            Res::Wish => {
                let p = Pos::from_code(code);
                let (s, slot) = (p.s(), p.i());
                let (t, amt) = self.sides[s].wish[slot];
                if t == 1 {
                    self.sides[s].wish[slot] = (0, 0);
                    if self.is_live(p) {
                        self.heal(p, amt as u32);
                    }
                } else if t > 1 {
                    self.sides[s].wish[slot] = (t - 1, amt);
                }
            }
            Res::Reflect => self.sides[code as usize].conds.reflect = self.sides[code as usize].conds.reflect.saturating_sub(1),
            Res::LightScreen => self.sides[code as usize].conds.light_screen = self.sides[code as usize].conds.light_screen.saturating_sub(1),
            Res::Safeguard => self.sides[code as usize].conds.safeguard = self.sides[code as usize].conds.safeguard.saturating_sub(1),
            Res::Mist => self.sides[code as usize].conds.mist = self.sides[code as usize].conds.mist.saturating_sub(1),
            Res::Tailwind => self.sides[code as usize].conds.tailwind = self.sides[code as usize].conds.tailwind.saturating_sub(1),
            Res::AuroraVeil => self.sides[code as usize].conds.aurora_veil = self.sides[code as usize].conds.aurora_veil.saturating_sub(1),
            Res::WideGuard => self.sides[code as usize].conds.wide_guard = false,
            Res::QuickGuard => self.sides[code as usize].conds.quick_guard = false,
            Res::TrickRoom => {
                if self.field.trick_room > 0 {
                    self.field.trick_room -= 1;
                    if self.field.trick_room == 0 {
                        blog!(self, "|-fieldend|move: Trick Room");
                    }
                }
            }
            Res::Gravity => {
                self.field.gravity = self.field.gravity.saturating_sub(1);
            }
            Res::Terrain => {
                if self.field.terrain != Terrain::None {
                    self.field.terrain_turns = self.field.terrain_turns.saturating_sub(1);
                    if self.field.terrain_turns == 0 {
                        self.clear_terrain();
                    }
                }
            }
            _ => {}
        }
    }

    fn weather_effect(&mut self, p: Pos, w: Weather) {
        let mh = self.m(p).max_hp as u32;
        let ab = self.ab(p);
        match w {
            Weather::Sand => {
                let m = self.m(p);
                let immune = m.has_type(Type::Rock)
                    || m.has_type(Type::Ground)
                    || m.has_type(Type::Steel)
                    || matches!(ab, Ab::SandVeil | Ab::SandRush | Ab::SandForce | Ab::Overcoat | Ab::MagicGuard);
                if !immune {
                    self.damage(p, (mh / 16).max(1), None, DmgKind::Indirect);
                }
            }
            Weather::Rain => match ab {
                Ab::RainDish => {
                    self.heal(p, (mh / 16).max(1));
                }
                Ab::DrySkin => {
                    self.heal(p, (mh / 8).max(1));
                }
                _ => {}
            },
            Weather::Sun => match ab {
                Ab::DrySkin | Ab::SolarPower => {
                    self.damage(p, (mh / 8).max(1), None, DmgKind::Indirect);
                }
                _ => {}
            },
            Weather::Snow => {
                if ab == Ab::IceBody {
                    self.heal(p, (mh / 16).max(1));
                }
            }
            Weather::None => {}
        }
    }

    /// Does this handler's duration run out now (fieldEvent then calls its end instead of
    /// the callback)? Read before the handler runs.
    fn residual_ends(&self, h: &Rh) -> bool {
        let side = |code: u8| &self.sides[code as usize].conds;
        match h.holder {
            Holder::Mon(code) => {
                let p = Pos::from_code(code);
                if h.slot {
                    return h.kind == Res::Wish && self.sides[p.s()].wish[p.i()].0 == 1;
                }
                let v = &self.m(p).vol;
                match h.kind {
                    Res::Flinch | Res::Protect | Res::Endure | Res::HelpingHand | Res::FollowMe | Res::Roost | Res::TwoTurnSpent => true,
                    Res::Stall => v.stall_dur == 1,
                    Res::Taunt => v.taunt == 1,
                    Res::Encore => v.encore == 1,
                    Res::Disable => v.disable == 1,
                    Res::MagnetRise => v.magnet_rise == 1,
                    Res::HealBlock => v.heal_block == 1,
                    Res::ThroatChop => v.throat_chop == 1,
                    Res::Yawn => v.yawn == 1,
                    Res::PerishSong => v.perish == 1,
                    Res::PartialTrap => v.partial_trap == 1,
                    _ => false,
                }
            }
            Holder::Side(s) => match h.kind {
                Res::Reflect => side(s).reflect == 1,
                Res::LightScreen => side(s).light_screen == 1,
                Res::Safeguard => side(s).safeguard == 1,
                Res::Mist => side(s).mist == 1,
                Res::Tailwind => side(s).tailwind == 1,
                Res::AuroraVeil => side(s).aurora_veil == 1,
                Res::WideGuard | Res::QuickGuard => true,
                _ => false,
            },
            Holder::Field => match h.kind {
                Res::Weather => self.field.weather != Weather::None && self.field.weather_turns == 1,
                Res::Terrain => self.field.terrain != Terrain::None && self.field.terrain_turns == 1,
                Res::TrickRoom => self.field.trick_room == 1,
                Res::Gravity => self.field.gravity == 1,
                _ => false,
            },
        }
    }

    fn residual_mon(&mut self, kind: Res, p: Pos) {
        let mh = self.m(p).max_hp as u32;
        match kind {
            Res::GrassyHeal => {
                if self.field.terrain == Terrain::Grassy && self.is_grounded(p) && self.m(p).vol.semi_inv == crate::state::SemiInv::None {
                    self.heal(p, (mh / 16).max(1));
                }
            }
            Res::Healer => {
                if let Some(a) = self.live_ally(p) {
                    // Champions data/mods/champions/abilities.ts: randomChance(1, 2).
                    if self.m(a).status != Status::None && self.rng.chance(1, 2) {
                        self.cure_status(a);
                    }
                }
            }
            Res::Leftovers => {
                if self.m(p).hp < self.m(p).max_hp {
                    self.reveal_item(p);
                }
                self.heal(p, (mh / 16).max(1));
            }
            Res::AquaRing | Res::Ingrain => {
                self.drain_heal(p, (mh / 16).max(1));
            }
            Res::LeechSeed => {
                // Nothing happens if the seeder's slot is empty or fainted;
                // whoever is in that slot gets the HP.
                let src = Pos::from_code(self.m(p).vol.leech_seed_src);
                if !self.is_live(src) {
                    return;
                }
                let dealt = self.damage(p, (mh / 8).max(1), Some(src), DmgKind::Indirect);
                if dealt > 0 && self.is_live(src) {
                    self.drain_heal(src, dealt);
                }
            }
            Res::Poison => {
                let st = self.m(p).status;
                let dmg = if st == Status::Tox {
                    let m = self.mm(p);
                    if m.status_turns < 15 {
                        m.status_turns += 1;
                    }
                    (mh / 16).max(1) * m.status_turns as u32
                } else {
                    (mh / 8).max(1)
                };
                self.damage(p, dmg, None, DmgKind::Indirect);
            }
            Res::Burn => {
                self.damage(p, (mh / 16).max(1), None, DmgKind::Indirect);
            }
            Res::Curse => {
                self.damage(p, (mh / 4).max(1), None, DmgKind::Indirect);
            }
            Res::SaltCure => {
                // Champions: 1/16 (1/8 for Water and Steel types).
                let m = self.m(p);
                let frac = if m.has_type(Type::Water) || m.has_type(Type::Steel) { 8 } else { 16 };
                self.damage(p, (mh / frac).max(1), None, DmgKind::Indirect);
            }
            Res::PartialTrap => {
                // Duration ticks first; then it ends if the trapper left.
                self.mm(p).vol.partial_trap -= 1;
                if self.m(p).vol.partial_trap == 0 {
                    return;
                }
                if !self.partial_trapper_active(p) {
                    self.mm(p).vol.partial_trap = 0;
                    return;
                }
                self.damage(p, (mh / 8).max(1), None, DmgKind::Indirect);
            }
            Res::Taunt => self.mm(p).vol.taunt -= 1,
            Res::Encore => {
                let m = self.mm(p);
                m.vol.encore -= 1;
                // Encore also ends if the move runs out of PP.
                let s = m.vol.encore_slot as usize;
                if m.pp[s] == 0 {
                    m.vol.encore = 0;
                }
            }
            Res::Disable => self.mm(p).vol.disable -= 1,
            Res::MagnetRise => self.mm(p).vol.magnet_rise -= 1,
            Res::HealBlock => self.mm(p).vol.heal_block -= 1,
            Res::ThroatChop => self.mm(p).vol.throat_chop -= 1,
            Res::Yawn => {
                let m = self.mm(p);
                m.vol.yawn -= 1;
                if m.vol.yawn == 0 {
                    self.try_set_status(p, Status::Slp, None);
                }
            }
            Res::PerishSong => {
                let m = self.mm(p);
                m.vol.perish -= 1;
                let left = m.vol.perish;
                blog!(self, "|-start|{}|perish{}", self.name(p), left);
                if left == 0 {
                    let hp = self.m(p).hp as u32;
                    self.damage(p, hp, None, DmgKind::SelfCost);
                }
            }
            Res::SpeedBoost => {
                // onResidual: only once the Pokemon has been in for a turn.
                if self.m(p).vol.active_turns == 0 {
                    return;
                }
                let mut up = [0i8; 7];
                up[B_SPE] = 1;
                self.reveal_ability(p);
                self.boost(p, &up, BoostSrc::SelfInflicted);
            }
            Res::Moody => {
                let boosts = self.m(p).boosts;
                let ups: crate::fixed::FixedVec<usize, 5> = (0..5).filter(|&i| boosts[i] < 6).collect();
                let mut b = [0i8; 7];
                if !ups.is_empty() {
                    let u = ups[self.rng.below(ups.len() as u32) as usize];
                    b[u] = 2;
                }
                let downs: crate::fixed::FixedVec<usize, 5> = (0..5).filter(|&i| boosts[i] > -6 && b[i] == 0).collect();
                if !downs.is_empty() {
                    let d = downs[self.rng.below(downs.len() as u32) as usize];
                    b[d] = -1;
                }
                self.reveal_ability(p);
                self.boost(p, &b, BoostSrc::SelfInflicted);
            }
            Res::Roost => self.mm(p).vol.roost = false,
            Res::WhiteHerb => self.check_white_herb(p),
            Res::Flinch => self.mm(p).vol.flinch = false,
            Res::Protect => self.mm(p).vol.protect = ProtectKind::None,
            Res::Endure => self.mm(p).vol.endure = false,
            Res::HelpingHand => self.mm(p).vol.helping_hand = 0,
            Res::FollowMe => self.mm(p).vol.follow_me = 0,
            Res::Stall => {
                let v = &mut self.mm(p).vol;
                v.stall_dur -= 1;
                if v.stall_dur == 0 {
                    v.stall_counter = 0;
                }
            }
            // twoturnmove / mustrecharge count down; the move action ends them.
            Res::TwoTurnMove | Res::MustRecharge => {}
            Res::TwoTurnSpent => self.mm(p).vol.twoturn_spent = false,
            _ => {}
        }
    }
}

fn holder_kind(h: Holder) -> Hk {
    match h {
        Holder::Mon(_) => Hk::Pokemon,
        Holder::Side(_) => Hk::Side,
        Holder::Field => Hk::Field,
    }
}

fn holder_key(h: Holder) -> (u8, u8) {
    match h {
        Holder::Field => (0, 0),
        Holder::Side(s) => (1, s),
        Holder::Mon(c) => (2, c),
    }
}
