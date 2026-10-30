//! Stat changes, status, volatiles, damage, healing, items, fainting,
//! weather/terrain and Mega Evolution.

use super::{blog, Battle};
use crate::dex::{Boosts, Category, Status, Terrain, VolKind, Weather, B_ACC, B_ATK, B_DEF, B_SPA, B_SPE, B_SPD};
use crate::kinds::{Ab, It};
use crate::state::{Pos, ProtectKind};
use crate::types::Type;

/// Where a stat change comes from (for ability interactions).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BoostSrc {
    /// The Pokemon's own move / ability / item.
    SelfInflicted,
    /// An opponent's (or ally's) move effect.
    Move(Pos),
    /// Intimidate from a Pokemon.
    Intimidate(Pos),
    /// Other ability (Cotton Down, etc.) from a Pokemon.
    Ability(Pos),
}

impl BoostSrc {
    fn source(self) -> Option<Pos> {
        match self {
            BoostSrc::SelfInflicted => None,
            BoostSrc::Move(p) | BoostSrc::Intimidate(p) | BoostSrc::Ability(p) => Some(p),
        }
    }
}

/// Kinds of damage for Magic Guard / Sturdy / Focus Sash rules.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DmgKind {
    /// A move's direct hit.
    Attack,
    /// Residual, recoil, Life Orb, Rough Skin... (Magic Guard blocks).
    Indirect,
    /// Confusion self-hit (a move-like hit; Magic Guard does not block).
    Confusion,
    /// HP cost the Pokemon pays itself (Substitute, Belly Drum, Steel Beam).
    SelfCost,
}

const STAT_NAMES: [&str; 7] = ["atk", "def", "spa", "spd", "spe", "accuracy", "evasion"];

impl Battle {
    // ---- boosts ------------------------------------------------------------------

    /// Apply stat stage changes. Returns true if any stat changed.
    pub fn boost(&mut self, tgt: Pos, boosts: &Boosts, src: BoostSrc) -> bool {
        if !self.is_live(tgt) {
            return false;
        }
        // Gen 6+: no boosts once every foe has fainted.
        if !self.foe_pokemon_left(tgt.s()) {
            return false;
        }
        let mut b = *boosts;
        let from_other = match src.source() {
            Some(s) => s != tgt,
            None => false,
        };
        let tab = self.tab(tgt);
        // ChangeBoost: Contrary inverts.
        if tab == Ab::Contrary {
            for x in b.iter_mut() {
                *x = -*x;
            }
        }
        // TryBoost: drop prevention from others.
        if from_other {
            let mut blocked_all = false;
            match tab {
                Ab::ClearBody | Ab::WhiteSmoke | Ab::FullMetalBody => blocked_all = true,
                Ab::HyperCutter => {
                    if b[B_ATK] < 0 {
                        b[B_ATK] = 0;
                    }
                }
                Ab::BigPecks => {
                    if b[B_DEF] < 0 {
                        b[B_DEF] = 0;
                    }
                }
                Ab::Illuminate | Ab::KeenEye => {
                    if b[B_ACC] < 0 {
                        b[B_ACC] = 0;
                    }
                }
                Ab::MirrorArmor => {
                    // Reflect drops back to the source.
                    let mut reflected = [0i8; 7];
                    let mut any = false;
                    for i in 0..7 {
                        if b[i] < 0 {
                            reflected[i] = b[i];
                            b[i] = 0;
                            any = true;
                        }
                    }
                    if any {
                        if let Some(s) = src.source() {
                            if self.is_live(s) && self.ab(s) != Ab::MirrorArmor {
                                blog!(self, "|-ability|{}|Mirror Armor", self.name(tgt));
                                self.boost(s, &reflected, BoostSrc::Ability(tgt));
                            }
                        }
                    }
                }
                _ => {}
            }
            if blocked_all {
                for x in b.iter_mut() {
                    if *x < 0 {
                        *x = 0;
                    }
                }
            }
            // Flower Veil protects Grass-type allies (and itself).
            if self.m(tgt).has_type(Type::Grass) {
                let veiled = tab == Ab::FlowerVeil || self.live_ally(tgt).map(|a| self.tab(a) == Ab::FlowerVeil).unwrap_or(false);
                if veiled {
                    for x in b.iter_mut() {
                        if *x < 0 {
                            *x = 0;
                        }
                    }
                }
            }
            if self.sides[tgt.s()].conds.mist > 0 && !self.attacker_infiltrates(src.source()) {
                for x in b.iter_mut() {
                    if *x < 0 {
                        *x = 0;
                    }
                }
            }
        }
        // Each stat is applied in turn; Defiant / Competitive react to each
        // stat an opponent lowered (Showdown's AfterEachBoost).
        let foe_src = src.source().map(|s| s.side != tgt.side).unwrap_or(false);
        let mut changed = false;
        let mut lowered = false;
        let mut raised = false;
        for i in 0..7 {
            if b[i] == 0 || !self.is_live(tgt) {
                continue;
            }
            let m = self.mm(tgt);
            let old = m.boosts[i];
            let new = (old as i32 + b[i] as i32).clamp(-6, 6) as i8;
            m.boosts[i] = new;
            let delta = new - old;
            if delta == 0 {
                continue;
            }
            changed = true;
            if b[i] < 0 {
                lowered = true;
            } else {
                raised = true;
            }
            blog!(self, "|{}|{}|{}|{}", if delta > 0 { "-boost" } else { "-unboost" }, self.name(tgt), STAT_NAMES[i], delta.abs());
            if b[i] < 0 && foe_src {
                let up_stat = match self.ab(tgt) {
                    Ab::Defiant => Some(B_ATK),
                    Ab::Competitive => Some(B_SPA),
                    _ => None,
                };
                if let Some(st) = up_stat {
                    self.reveal_ability(tgt);
                    let mut up = [0i8; 7];
                    up[st] = 2;
                    self.boost(tgt, &up, BoostSrc::SelfInflicted);
                }
            }
        }
        if raised {
            self.mm(tgt).vol.stats_raised_this_turn = true;
        }
        if lowered {
            self.mm(tgt).vol.stats_lowered_this_turn = true;
        }
        changed
    }

    fn attacker_infiltrates(&self, src: Option<Pos>) -> bool {
        match src {
            Some(s) if self.is_live(s) => self.ab(s) == Ab::Infiltrator,
            _ => false,
        }
    }

    /// White Herb: restore lowered stats (Update event).
    pub fn check_white_herb(&mut self, p: Pos) {
        if !self.is_live(p) || self.it(p) != It::WhiteHerb {
            return;
        }
        if self.m(p).boosts.iter().any(|&x| x < 0) {
            for x in self.mm(p).boosts.iter_mut() {
                if *x < 0 {
                    *x = 0;
                }
            }
            blog!(self, "|-enditem|{}|White Herb", self.name(p));
            self.consume_item(p);
        }
    }

    /// Intimidate on all adjacent foes.
    pub fn intimidate(&mut self, src: Pos) {
        self.reveal_ability(src);
        blog!(self, "|-ability|{}|Intimidate|boost", self.name(src));
        let foes: crate::fixed::FixedVec<Pos, 2> = self.live_foes(src).collect();
        for &t in &foes {
            if self.m(t).vol.substitute > 0 {
                continue;
            }
            match self.tab(t) {
                Ab::InnerFocus | Ab::Oblivious | Ab::OwnTempo | Ab::Scrappy => {
                    self.reveal_ability(t);
                    blog!(self, "|-fail|{}|unboost|[from] ability", self.name(t));
                    continue;
                }
                _ => {}
            }
            let mut d = [0i8; 7];
            d[B_ATK] = -1;
            self.boost(t, &d, BoostSrc::Intimidate(src));
            if self.is_live(t) && self.ab(t) == Ab::Rattled {
                let mut up = [0i8; 7];
                up[B_SPE] = 1;
                self.boost(t, &up, BoostSrc::SelfInflicted);
            }
        }
    }

    // ---- status ------------------------------------------------------------------

    /// Can `st` be inflicted on `tgt`? (type, ability, terrain, Safeguard).
    pub fn status_immune(&self, tgt: Pos, st: Status, src: Option<Pos>) -> bool {
        let m = self.m(tgt);
        let corrosion = src.map(|s| self.is_live(s) && self.ab(s) == Ab::Corrosion).unwrap_or(false);
        match st {
            Status::Par if m.has_type(Type::Electric) => return true,
            Status::Brn if m.has_type(Type::Fire) => return true,
            Status::Frz if m.has_type(Type::Ice) => return true,
            // Sunny Day's onImmunity.
            Status::Frz if self.weather() == Weather::Sun => return true,
            Status::Psn | Status::Tox if (m.has_type(Type::Poison) || m.has_type(Type::Steel)) && !corrosion => {
                return true
            }
            _ => {}
        }
        let from_other = src.map(|s| s != tgt).unwrap_or(false);
        match (self.tab(tgt), st) {
            (Ab::Limber, Status::Par) => return true,
            (Ab::Insomnia | Ab::VitalSpirit, Status::Slp) => return true,
            (Ab::WaterBubble | Ab::ThermalExchange, Status::Brn) => return true,
            (Ab::MagmaArmor, Status::Frz) => return true,
            (Ab::LeafGuard, _) if self.weather() == Weather::Sun => return true,
            _ => {}
        }
        if st == Status::Slp {
            if self.tab(tgt) == Ab::SweetVeil || self.live_ally(tgt).map(|a| self.tab(a) == Ab::SweetVeil).unwrap_or(false) {
                return true;
            }
        }
        if from_other && m.has_type(Type::Grass) {
            let veiled = self.tab(tgt) == Ab::FlowerVeil
                || self.live_ally(tgt).map(|a| self.tab(a) == Ab::FlowerVeil).unwrap_or(false);
            if veiled {
                return true;
            }
        }
        if self.is_grounded(tgt) && m.vol.semi_inv == crate::state::SemiInv::None {
            match self.field.terrain {
                Terrain::Misty => return true,
                Terrain::Electric if st == Status::Slp => return true,
                _ => {}
            }
        }
        if from_other && self.sides[tgt.s()].conds.safeguard > 0 && !self.attacker_infiltrates(src) {
            return true;
        }
        false
    }

    /// Try to inflict a major status. Returns true on success.
    pub fn try_set_status(&mut self, tgt: Pos, st: Status, src: Option<Pos>) -> bool {
        if !self.is_live(tgt) || st == Status::None {
            return false;
        }
        if self.m(tgt).status != Status::None {
            return false;
        }
        if self.status_immune(tgt, st, src) {
            return false;
        }
        let turns = match st {
            Status::Slp => {
                // Champions: sleep lasts 2 or 3 (startTime sampled from [2, 3, 3]).
                let t = [2u8, 3, 3][self.rng.below(3) as usize];
                t
            }
            Status::Frz => 3,
            _ => 0,
        };
        {
            let m = self.mm(tgt);
            m.status = st;
            m.status_turns = turns;
        }
        blog!(self, "|-status|{}|{:?}", self.name(tgt), st);
        // Synchronize.
        if let Some(s) = src {
            if s.side != tgt.side
                && self.ab(tgt) == Ab::Synchronize
                && matches!(st, Status::Brn | Status::Par | Status::Psn | Status::Tox)
                && self.is_live(s)
            {
                self.reveal_ability(tgt);
                let st2 = st;
                self.try_set_status(s, st2, Some(tgt));
            }
        }
        self.check_status_berry(tgt);
        true
    }

    pub fn cure_status(&mut self, p: Pos) {
        let m = self.mm(p);
        m.status = Status::None;
        m.status_turns = 0;
    }

    /// Lum / Chesto / etc. (Update event).
    pub fn check_status_berry(&mut self, p: Pos) {
        if !self.is_live(p) {
            return;
        }
        let st = self.m(p).status;
        let conf = self.m(p).vol.confusion > 0;
        let cure = match self.it(p) {
            It::LumBerry => st != Status::None || conf,
            It::ChestoBerry => st == Status::Slp,
            It::CheriBerry => st == Status::Par,
            It::PechaBerry => matches!(st, Status::Psn | Status::Tox),
            It::RawstBerry => st == Status::Brn,
            It::AspearBerry => st == Status::Frz,
            It::PersimBerry => conf,
            _ => false,
        };
        if cure && !self.unnerved(p) {
            match self.it(p) {
                It::LumBerry => {
                    self.cure_status(p);
                    self.mm(p).vol.confusion = 0;
                }
                It::PersimBerry => self.mm(p).vol.confusion = 0,
                _ => self.cure_status(p),
            }
            blog!(self, "|-enditem|{}|berry|[eat]", self.name(p));
            self.eat_item(p);
        }
    }

    // ---- volatiles ---------------------------------------------------------------------

    /// Add a volatile status. Returns true on success.
    /// Aroma Veil's onAllyTryAddVolatile: the holder and its ally block six volatiles.
    pub(crate) fn aroma_veil(&self, t: Pos) -> bool {
        self.tab(t) == Ab::AromaVeil
            || self.live_ally(t).map(|a| self.tab(a) == Ab::AromaVeil).unwrap_or(false)
    }

    pub fn add_volatile(&mut self, tgt: Pos, v: VolKind, src: Option<Pos>) -> bool {
        if !self.is_live(tgt) {
            return false;
        }
        let tab = self.tab(tgt);
        let grounded_misty =
            self.field.terrain == Terrain::Misty && self.is_grounded(tgt) && self.m(tgt).vol.semi_inv == crate::state::SemiInv::None;
        match v {
            // Aroma Veil blocks these six before any of their side effects (Encore's
            // queued-action change, Mental Herb consumption, ...).
            VolKind::Taunt | VolKind::Disable | VolKind::Encore | VolKind::HealBlock
            | VolKind::Torment | VolKind::Attract
                if self.aroma_veil(tgt) =>
            {
                return false;
            }
            VolKind::Flinch => {
                if tab == Ab::InnerFocus {
                    return false;
                }
                self.mm(tgt).vol.flinch = true;
            }
            VolKind::Confusion => {
                if self.m(tgt).vol.confusion > 0 || tab == Ab::OwnTempo || grounded_misty {
                    return false;
                }
                if src.map(|s| s != tgt).unwrap_or(false)
                    && self.sides[tgt.s()].conds.safeguard > 0
                    && !self.attacker_infiltrates(src)
                {
                    return false;
                }
                let t = self.rng.range(2, 6) as u8;
                self.mm(tgt).vol.confusion = t;
                blog!(self, "|-start|{}|confusion", self.name(tgt));
                self.check_status_berry(tgt);
            }
            VolKind::MustRecharge => self.mm(tgt).vol.must_recharge = true,
            VolKind::Minimize => {
                // onRestart returns null: a second Minimize keeps the volatile but fails to add.
                if self.m(tgt).vol.minimize {
                    return false;
                }
                self.mm(tgt).vol.minimize = true;
            }
            VolKind::Taunt => {
                if self.m(tgt).vol.taunt > 0 || tab == Ab::Oblivious {
                    return false;
                }
                let mut d = 3;
                if self.m(tgt).vol.active_turns > 0 && self.queue.will_move(tgt).is_none() {
                    d += 1;
                }
                self.mm(tgt).vol.taunt = d;
                blog!(self, "|-start|{}|move: Taunt", self.name(tgt));
                if self.it(tgt) == It::MentalHerb {
                    self.mm(tgt).vol.taunt = 0;
                    self.consume_item(tgt);
                }
            }
            VolKind::Yawn => {
                if self.m(tgt).vol.yawn > 0 || self.m(tgt).status != Status::None {
                    return false;
                }
                if self.status_immune(tgt, Status::Slp, src) {
                    return false;
                }
                self.mm(tgt).vol.yawn = 2;
            }
            VolKind::FocusEnergy => {
                if self.m(tgt).vol.crit_stage >= 2 {
                    return false;
                }
                self.mm(tgt).vol.crit_stage = 2;
            }
            VolKind::DragonCheer => {
                if self.m(tgt).vol.crit_stage > 0 {
                    return false;
                }
                self.mm(tgt).vol.crit_stage = if self.m(tgt).has_type(Type::Dragon) { 2 } else { 1 };
            }
            VolKind::LeechSeed => {
                if self.m(tgt).has_type(Type::Grass) || self.m(tgt).vol.leech_seed {
                    return false;
                }
                let code = src.map(|s| s.code()).unwrap_or(0);
                let m = self.mm(tgt);
                m.vol.leech_seed = true;
                m.vol.leech_seed_src = code;
            }
            VolKind::Roost => self.mm(tgt).vol.roost = true,
            VolKind::Imprison => {
                if self.m(tgt).vol.imprison {
                    return false;
                }
                self.mm(tgt).vol.imprison = true;
            }
            VolKind::GlaiveRush => self.mm(tgt).vol.glaive_rush = 1,
            VolKind::SaltCure => {
                if self.m(tgt).vol.salt_cure {
                    return false;
                }
                self.mm(tgt).vol.salt_cure = true;
            }
            VolKind::Curse => self.mm(tgt).vol.curse = true,
            VolKind::Torment => {
                if self.m(tgt).vol.torment {
                    return false;
                }
                self.mm(tgt).vol.torment = true;
            }
            VolKind::HealBlock => {
                if self.m(tgt).vol.heal_block > 0 {
                    return false;
                }
                self.mm(tgt).vol.heal_block = 5;
            }
            VolKind::MagnetRise => {
                if self.m(tgt).vol.magnet_rise > 0 || self.m(tgt).vol.smacked_down || self.m(tgt).vol.ingrain {
                    return false;
                }
                self.mm(tgt).vol.magnet_rise = 5;
            }
            VolKind::AquaRing => {
                if self.m(tgt).vol.aqua_ring {
                    return false;
                }
                self.mm(tgt).vol.aqua_ring = true;
            }
            VolKind::Ingrain => {
                if self.m(tgt).vol.ingrain {
                    return false;
                }
                self.mm(tgt).vol.ingrain = true;
            }
            VolKind::Charge => self.mm(tgt).vol.charge = true,
            VolKind::LaserFocus => self.mm(tgt).vol.laser_focus = 2,
            VolKind::TarShot => {
                if self.m(tgt).vol.tar_shot {
                    return false;
                }
                self.mm(tgt).vol.tar_shot = true;
            }
            VolKind::SmackDown => {
                // The volatile only starts on a Pokemon that is in the air.
                let m = self.m(tgt);
                let mut applies = (m.has_type(Type::Flying) || self.ab(tgt) == Ab::Levitate)
                    && !(self.it(tgt) == It::IronBall || m.vol.ingrain || self.field.gravity > 0);
                if m.vol.semi_inv == crate::state::SemiInv::Air {
                    // Knocked out of Fly / Bounce: the move is cancelled.
                    applies = true;
                    let mm = self.mm(tgt);
                    mm.vol.semi_inv = crate::state::SemiInv::None;
                    mm.vol.charging = 0;
                    self.queue.remove_where(|a| a.pos == tgt && matches!(a.kind, super::ActionKind::Move { .. }));
                }
                if self.m(tgt).vol.magnet_rise > 0 {
                    applies = true;
                }
                if !applies {
                    return false;
                }
                let m = self.mm(tgt);
                m.vol.smacked_down = true;
                m.vol.magnet_rise = 0;
            }
            VolKind::NoRetreat => {
                if self.m(tgt).vol.no_retreat {
                    return false;
                }
                self.mm(tgt).vol.no_retreat = true;
            }
            VolKind::Attract => {
                let (sg, tg) = (src.map(|s| self.m(s).gender).unwrap_or(0), self.m(tgt).gender);
                if sg == 0 || tg == 0 || sg == tg || self.m(tgt).vol.attract || tab == Ab::Oblivious {
                    return false;
                }
                let s = src.unwrap();
                let mon = self.sides[s.s()].active[s.i()];
                let v = &mut self.mm(tgt).vol;
                v.attract = true;
                v.attract_src = s.code();
                v.attract_mon = mon;
            }
            VolKind::Stockpile => {
                if self.m(tgt).vol.stockpile >= 3 {
                    return false;
                }
                self.mm(tgt).vol.stockpile += 1;
                let mut up = [0i8; 7];
                up[B_DEF] = 1;
                up[B_SPD] = 1;
                self.boost(tgt, &up, BoostSrc::SelfInflicted);
            }
            VolKind::PartiallyTrapped => {
                // Fire Spin, Whirlpool, Infestation...: 5 or 6 turns.
                if self.m(tgt).vol.partial_trap > 0 {
                    return false;
                }
                let s = src.unwrap_or(tgt);
                let mon = self.sides[s.s()].active[s.i()];
                let dur = self.rng.range(5, 7) as u8;
                let m = self.mm(tgt);
                m.vol.partial_trap = dur;
                m.vol.partial_trap_src = s.code();
                m.vol.partial_trap_mon = mon;
            }
            VolKind::Endure => self.mm(tgt).vol.endure = true,
            VolKind::DestinyBond => self.mm(tgt).vol.destiny_bond = true,
            VolKind::Protect => self.mm(tgt).vol.protect = ProtectKind::Protect,
            // Volatiles applied by scripted moves themselves.
            VolKind::Encore
            | VolKind::Disable
            | VolKind::Substitute
            | VolKind::HelpingHand
            | VolKind::FollowMe
            | VolKind::RagePowder
            | VolKind::ThroatChop
            | VolKind::Unsupported => return false,
        }
        true
    }

    // ---- HP ------------------------------------------------------------------------------

    /// Deal damage. Returns damage actually dealt.
    pub fn damage(&mut self, tgt: Pos, amount: u32, src: Option<Pos>, kind: DmgKind) -> u32 {
        if !self.is_live(tgt) || amount == 0 {
            return 0;
        }
        if kind == DmgKind::Indirect && self.ab(tgt) == Ab::MagicGuard {
            return 0;
        }
        let mut dmg = amount.min(self.m(tgt).hp as u32);
        // Disguise (onDamage priority 1): a move's hit on an intact Mimikyu is nullified
        // and busts the disguise; the Update event then changes the forme and pays
        // baseMaxhp / 8. The -damage line still follows with unchanged HP, and the
        // Pokemon does not count as hurt this turn.
        if kind == DmgKind::Attack && self.tab(tgt) == Ab::Disguise && self.disguise_intact(tgt) {
            self.mm(tgt).vol.disguise_busted = true;
            blog!(self, "|-activate|{}|ability: Disguise", self.name(tgt));
            dmg = 0;
        }
        if kind == DmgKind::Attack {
            let m = self.m(tgt);
            let full = m.hp == m.max_hp;
            if dmg >= m.hp as u32 {
                if m.vol.endure {
                    dmg = m.hp as u32 - 1;
                    blog!(self, "|-activate|{}|move: Endure", self.name(tgt));
                } else if full && m.hp > 1 && self.tab(tgt) == Ab::Sturdy {
                    dmg = m.hp as u32 - 1;
                    self.reveal_ability(tgt);
                    blog!(self, "|-activate|{}|ability: Sturdy", self.name(tgt));
                } else if full && m.hp > 1 && self.it(tgt) == It::FocusSash {
                    dmg = m.hp as u32 - 1;
                    blog!(self, "|-enditem|{}|Focus Sash", self.name(tgt));
                    self.consume_item(tgt);
                }
            }
        }
        let m = self.mm(tgt);
        m.hp -= dmg as u16;
        if kind == DmgKind::Attack && dmg > 0 {
            m.vol.hurt_this_turn = true;
        }
        let hp = m.hp;
        blog!(self, "|-damage|{}|{}/{}", self.name(tgt), hp, self.m(tgt).max_hp);
        if hp == 0 {
            self.queue_faint(tgt);
        }
        let _ = src;
        dmg
    }

    pub fn heal(&mut self, tgt: Pos, amount: u32) -> u32 {
        if !self.is_live(tgt) || amount == 0 {
            return 0;
        }
        if self.m(tgt).vol.heal_block > 0 {
            return 0;
        }
        let m = self.mm(tgt);
        let room = (m.max_hp - m.hp) as u32;
        let h = amount.min(room);
        m.hp += h as u16;
        if h > 0 {
            blog!(self, "|-heal|{}|{}/{}", self.name(tgt), self.m(tgt).hp, self.m(tgt).max_hp);
        }
        h
    }

    /// Sitrus Berry & co. after HP changes (Update event).
    pub fn check_hp_berry(&mut self, p: Pos) {
        if !self.is_live(p) || !matches!(self.it(p), It::SitrusBerry | It::OranBerry) {
            return;
        }
        let m = self.m(p);
        // onTryEatItem: a healing berry is not eaten under Heal Block.
        if (m.hp as u32) * 2 > m.max_hp as u32 || self.unnerved(p) || m.vol.heal_block > 0 {
            return;
        }
        let m = self.m(p);
        match self.it(p) {
            It::SitrusBerry if (m.hp as u32) * 2 <= m.max_hp as u32 => {
                let amt = m.max_hp as u32 / 4;
                blog!(self, "|-enditem|{}|Sitrus Berry|[eat]", self.name(p));
                self.eat_item(p);
                self.heal(p, amt);
            }
            It::OranBerry if (m.hp as u32) * 2 <= m.max_hp as u32 => {
                self.eat_item(p);
                self.heal(p, 10);
            }
            _ => {}
        }
    }

    /// update_items(p) may change something: a superset of its conditions (attract, a
    /// healing berry at half HP or less, a status berry with a status or confusion, the
    /// Disguise forme change). Must be widened whenever update_items gains a check.
    pub(crate) fn update_can_act(&self, m: &crate::state::Mon) -> bool {
        if m.vol.attract {
            return true;
        }
        if self.dex.ab(m.ability) == Ab::Disguise
            && m.vol.disguise_busted
            && m.alt_species != 0
            && m.species != m.alt_species
            && self.dex.species_id("mimikyu") == Some(m.base_species)
        {
            return true;
        }
        if m.item == 0 {
            return false;
        }
        match self.dex.it(m.item) {
            // Mental Herb clears attract / taunt / encore / torment / disable / healblock.
            It::MentalHerb => {
                m.vol.taunt > 0 || m.vol.encore > 0 || m.vol.disable > 0 || m.vol.heal_block > 0 || m.vol.torment || m.vol.attract
            }
            It::SitrusBerry | It::OranBerry => (m.hp as u32) * 2 <= m.max_hp as u32,
            It::LumBerry | It::ChestoBerry | It::CheriBerry | It::PechaBerry | It::RawstBerry | It::AspearBerry | It::PersimBerry => {
                m.status != Status::None || m.vol.confusion > 0
            }
            _ => false,
        }
    }

    /// All Update-event item checks for one Pokemon.
    pub fn update_items(&mut self, p: Pos) {
        // attract's onUpdate (a condition, before the item): it ends once its source is
        // no longer active.
        let v = &self.m(p).vol;
        if v.attract {
            let s = Pos::from_code(v.attract_src);
            let i = self.sides[s.s()].active[s.i()];
            if i != v.attract_mon || self.sides[s.s()].mons[i as usize].fainted {
                self.mm(p).vol.attract = false;
            }
        }
        // Disguise's onUpdate (an ability: after conditions, before the item): the busted
        // disguise changes the forme for good and pays baseMaxhp / 8. Unlike the attacking
        // events, the Update event runs a suppressed ability's handler as well.
        if self.ab(p) == Ab::Disguise && self.disguise_bust_pending(p) {
            let alt = self.dex.sp(self.m(p).alt_species);
            let name = alt.name.clone();
            blog!(self, "|-formechange|{}|{}|[from] ability: Disguise", self.name(p), name);
            self.mm(p).set_alt_forme(true);
            self.mm(p).alt_locked = true;
            let eighth = (self.m(p).max_hp / 8) as u32;
            self.damage(p, eighth, Some(p), DmgKind::Indirect);
        }
        if self.m(p).item == 0 {
            return;
        }
        self.check_hp_berry(p);
        // Mental Herb (onUpdate): if any of the six conditions is present, all six are
        // removed and the item is consumed.
        if self.it(p) == It::MentalHerb {
            let v = self.m(p).vol;
            if v.attract || v.taunt > 0 || v.encore > 0 || v.disable > 0 || v.heal_block > 0 || v.torment {
                blog!(self, "|-enditem|{}|Mental Herb", self.name(p));
                let m = self.mm(p);
                m.vol.attract = false;
                m.vol.taunt = 0;
                m.vol.encore = 0;
                m.vol.disable = 0;
                m.vol.heal_block = 0;
                m.vol.torment = false;
                self.consume_item(p);
            }
        }
        // A consumed HP berry may be replaced by Symbiosis, so inspect the current
        // state after that handler rather than caching the held item across it.
        if self.m(p).status != Status::None || self.m(p).vol.confusion > 0 {
            self.check_status_berry(p);
        }
    }

    /// runEvent('AfterMove' / 'AfterMega', user): White Herb's onAnyAfterMove /
    /// onAnyAfterMega on every active holder, sorted by cached speed (equal speeds shuffle).
    /// The user's Charge volatile (onAfterMove) only takes its place in the sort.
    pub(crate) fn white_herb_event(&mut self, user: Pos, after_move: bool) {
        let mut h = [(0i32, 0u8, 0u8); 6];
        let mut n = 0;
        if after_move && self.m(user).vol.charge {
            h[n] = (self.m(user).speed, 2, 4);
            n += 1;
        }
        for q in [Pos::new(user.s(), 0), Pos::new(user.s(), 1), user.foe(0), user.foe(1)] {
            if self.is_live(q) && self.m(q).item != 0 && self.dex.it(self.m(q).item) == It::WhiteHerb {
                h[n] = (self.m(q).speed, 8, q.code());
                n += 1;
            }
        }
        if n == 0 {
            return;
        }
        crate::rng::speed_sort(&mut self.rng, &mut h[..n], |a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for &(_, _, c) in &h[..n] {
            if c < 4 {
                self.check_white_herb(Pos::from_code(c));
            }
        }
    }

    // ---- items --------------------------------------------------------------------------

    /// Remove the held item (used up or knocked off).
    pub fn remove_item(&mut self, p: Pos) {
        let had = self.m(p).item;
        if had == 0 {
            return;
        }
        {
            let m = self.mm(p);
            m.last_item = had;
            m.item = 0;
            m.reveal.item = true;
        }
        if self.ab(p) == Ab::Unburden {
            self.mm(p).vol.unburden = true;
        }
    }

    /// Showdown's setItem: the new item's onStart runs, and a Choice item's ends any
    /// choicelock (a lock otherwise outlives the item that set it).
    pub fn set_item(&mut self, p: Pos, it: crate::dex::ItemId) {
        let choice = super::legal::is_choice(self.dex.it(it));
        let m = self.mm(p);
        m.item = it;
        if choice {
            m.vol.choice_lock = 0;
        }
    }

    /// Showdown's takeItem (Magician, Pickpocket): the item leaves its holder
    /// without counting as used (no Recycle), unless it is a Mega Stone.
    pub fn take_item(&mut self, p: Pos) -> Option<crate::dex::ItemId> {
        if !self.is_live(p) {
            return None;
        }
        let it = self.m(p).item;
        if it == 0 || self.dex.item(it).is_mega_stone {
            return None;
        }
        {
            let m = self.mm(p);
            m.item = 0;
            m.reveal.item = true;
        }
        if self.ab(p) == Ab::Unburden {
            self.mm(p).vol.unburden = true;
        }
        blog!(self, "|-enditem|{}|{}|[silent]", self.name(p), self.dex.item(it).name);
        Some(it)
    }

    pub fn consume_item(&mut self, p: Pos) {
        self.remove_item(p);
        self.symbiosis(p);
    }

    pub fn eat_item(&mut self, p: Pos) {
        self.mm(p).ate_berry = true;
        self.consume_item(p);
        // Cheek Pouch's onEatItem: a third of the base max HP.
        if self.is_live(p) && self.ab(p) == Ab::CheekPouch {
            self.reveal_ability(p);
            let h = self.m(p).max_hp as u32 / 3;
            self.heal(p, h);
        }
    }

    fn symbiosis(&mut self, p: Pos) {
        if let Some(a) = self.live_ally(p) {
            if self.ab(a) == Ab::Symbiosis && self.m(a).item != 0 && self.m(p).item == 0 {
                let it = self.m(a).item;
                if self.dex.item(it).is_mega_stone {
                    return;
                }
                self.mm(a).item = 0;
                self.set_item(p, it);
                self.reveal_ability(a);
            }
        }
    }

    // ---- faint ---------------------------------------------------------------------------

    pub(crate) fn queue_faint(&mut self, p: Pos) {
        // Faints are processed after the action; record the order they hit 0.
        self.last_faint_side = Some(p.side);
        let _ = p;
    }

    pub fn faint(&mut self, p: Pos) {
        let name = self.name(p);
        // faintMessages runs the ability's End before the Pokemon counts as fainted.
        if matches!(self.ab(p), Ab::AirLock | Ab::CloudNine) {
            self.each_event_draws();
        }
        {
            let side = &mut self.sides[p.s()];
            side.total_fainted = side.total_fainted.saturating_add(1);
            side.fainted_this_turn = true;
            let m = side.mons.get_mut(side.active[p.i()] as usize).unwrap();
            m.fainted = true;
            m.hp = 0;
            m.status_before_faint = m.status;
            m.status = Status::None;
            m.boosts = [0; 7];
            m.vol = Default::default();
            // clearVolatile: ability and types back to the (Mega) forme's own.
            m.ability = m.base_ability;
            m.types = m.base_types;
            m.revert_alt_forme();
            // clearVolatile -> setSpecies(baseSpecies): the raw Speed stat.
            m.speed = m.fainted_speed() as i32;
        }
        blog!(self, "|faint|{}", name);
        // The slot stays occupied by the fainted Pokemon until replaced.
    }

    // ---- field ----------------------------------------------------------------------------

    pub fn set_weather(&mut self, w: Weather, src: Pos) -> bool {
        if self.field.weather == w {
            return false;
        }
        let rock = match (w, self.it(src)) {
            (Weather::Rain, It::DampRock) | (Weather::Sun, It::HeatRock) | (Weather::Sand, It::SmoothRock) | (Weather::Snow, It::IcyRock) => true,
            _ => false,
        };
        self.field.weather = w;
        self.field.weather_turns = if rock { 8 } else { 5 };
        blog!(self, "|-weather|{:?}", w);
        // eachEvent('WeatherChange').
        self.each_event_draws();
        true
    }

    /// Showdown's `field.clearWeather`: FieldEnd, then eachEvent('WeatherChange').
    pub fn clear_weather(&mut self) -> bool {
        if self.field.weather == Weather::None {
            return false;
        }
        blog!(self, "|-weather|none");
        self.field.weather = Weather::None;
        self.field.weather_turns = 0;
        self.each_event_draws();
        true
    }

    /// Showdown's `field.clearTerrain`: FieldEnd, then eachEvent('TerrainChange').
    pub fn clear_terrain(&mut self) -> bool {
        if self.field.terrain == Terrain::None {
            return false;
        }
        blog!(self, "|-fieldend|{:?}", self.field.terrain);
        self.field.terrain = Terrain::None;
        self.field.terrain_turns = 0;
        self.each_event_draws();
        true
    }

    pub fn set_terrain(&mut self, t: Terrain, src: Pos) -> bool {
        if self.field.terrain == t {
            return false;
        }
        self.field.terrain = t;
        self.field.terrain_turns = if self.it(src) == It::TerrainExtender { 8 } else { 5 };
        blog!(self, "|-fieldstart|{:?}", t);
        // eachEvent('TerrainChange').
        self.each_event_draws();
        // Seeds activate when their terrain starts.
        let all: crate::fixed::FixedVec<Pos, 4> = self.live_positions().collect();
        for &p in &all {
            self.check_seed(p);
        }
        true
    }

    pub fn check_seed(&mut self, p: Pos) {
        if !self.is_live(p) {
            return;
        }
        let t = self.field.terrain;
        let stat = match (self.it(p), t) {
            (It::GrassySeed, Terrain::Grassy) | (It::ElectricSeed, Terrain::Electric) => B_DEF,
            (It::PsychicSeed, Terrain::Psychic) | (It::MistySeed, Terrain::Misty) => B_SPD,
            _ => return,
        };
        let mut up = [0i8; 7];
        up[stat] = 1;
        blog!(self, "|-enditem|{}|seed", self.name(p));
        self.consume_item(p);
        self.boost(p, &up, BoostSrc::SelfInflicted);
    }

    // ---- Mega Evolution -----------------------------------------------------------------

    pub fn mega_evolve(&mut self, p: Pos) {
        if !self.is_live(p) || !self.can_mega(p) {
            return;
        }
        let d = self.dex;
        {
            let m = self.mm(p);
            m.species = m.mega_species;
            m.types = m.mega_types;
            m.base_types = m.mega_types;
            for i in 1..6 {
                m.stats[i] = m.mega_stats[i];
            }
            m.weight_hg = m.mega_weight_hg;
            m.ability = m.mega_ability;
            m.base_ability = m.mega_ability;
            // setAbility starts a fresh abilityState (Protean may change the type again);
            // the old ability's End removes Flash Fire's volatile.
            m.vol.protean = false;
            if d.ab(m.mega_ability) != Ab::FlashFire {
                m.vol.flash_fire = false;
            }
            m.is_mega = true;
            m.reveal.item = true;
            m.reveal.ability = true;
            // formeChange -> setSpecies: the raw Speed stat.
            m.speed = m.stats[5] as i32;
        }
        self.sides[p.s()].mega_used = true;
        blog!(self, "|-mega|{}", self.name(p));
        // The new ability starts immediately.
        self.ability_start(p);
        self.white_herb_event(p, false);
    }

    pub fn reveal_ability(&mut self, p: Pos) {
        if self.is_live(p) {
            self.mm(p).reveal.ability = true;
        }
    }

    pub fn reveal_item(&mut self, p: Pos) {
        if self.is_live(p) {
            self.mm(p).reveal.item = true;
        }
    }

    /// Is this move a "status" move for Good as Gold / Magic Bounce purposes?
    pub fn is_status_move(&self, cat: Category) -> bool {
        cat == Category::Status
    }
}
