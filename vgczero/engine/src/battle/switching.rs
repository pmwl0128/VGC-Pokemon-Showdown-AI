//! Switching out and in, and switch-in effects.

use super::effects::{BoostSrc, DmgKind, LogFrom};
use super::{blog, Battle};
use crate::dex::{Status, Terrain, Weather, B_SPE};
use crate::kinds::{Ab, It};
use crate::state::{Pos, Vol};
use crate::types::Type;

/// Abilities Trace cannot copy.
const NO_TRACE: &[&str] = &[
    "trace", "illusion", "imposter", "stancechange", "zerotohero", "commander", "receiver",
    "powerofalchemy", "flowergift", "forecast", "hungerswitch", "multitype", "neutralizinggas",
    "rkssystem", "schooling", "shieldsdown", "zenmode", "disguise", "iceface", "gulpmissile",
    "battlebond", "comatose", "protosynthesis", "quarkdrive", "wonderguard",
];

impl Battle {
    /// Replace the Pokemon at `p` with team member `to` (no entry effects). `drag`: a forced
    /// switch (Roar, Red Card...), which pinned writes as `|drag|` with no `[from]`.
    pub fn switch_out_in(&mut self, p: Pos, to: usize, drag: bool) {
        let s = p.s();
        let old = self.sides[s].active[p.i()];
        // A self-switch move names itself in the switch line (pinned switchIn's sourceEffect);
        // read before the leaving Pokemon's volatiles are cleared.
        let from_move = if old != crate::state::NO_MON { self.sides[s].mons[old as usize].vol.switch_move } else { 0 };
        let mut baton: Option<(crate::dex::Boosts, Vol)> = None;
        if old != crate::state::NO_MON {
            let oi = old as usize;
            let alive = self.sides[s].mons[oi].alive();
            if alive {
                // Switch-out abilities.
                let ab = self.dex.ab(self.sides[s].mons[oi].ability);
                let regen_ident = self.name(p);
                let mut regen_line: Option<String> = None;
                let regen_shared = self.cfg.log_side.map(|ls| ls as usize != s).unwrap_or(false);
                {
                    let m = &mut self.sides[s].mons[oi];
                    match ab {
                        Ab::Regenerator => {
                            let h = (m.max_hp / 3).min(m.max_hp - m.hp);
                            if m.vol.heal_block == 0 {
                                m.hp += h;
                                if h > 0 {
                                    // Pinned champions regenerator: a silent heal line.
                                    regen_line = Some(format!("|-heal|{}|{}|[from] ability: Regenerator|[silent]", regen_ident, if regen_shared { Self::hp_text_shared_mon(m) } else { Self::hp_text_mon(m) }));
                                }
                            }
                        }
                        Ab::NaturalCure => {
                            m.status = Status::None;
                            m.status_turns = 0;
                        }
                        // Zero to Hero: Palafin becomes Palafin-Hero for good.
                        Ab::ZeroToHero if m.alt_species != 0 && m.species != m.alt_species => {
                            m.set_alt_forme(true);
                            m.alt_locked = true;
                        }
                        _ => {}
                    }
                }
                if let Some(l) = regen_line {
                    blog!(self, "{}", l);
                }
                // The ability's End (Air Lock / Cloud Nine: eachEvent('WeatherChange')) while
                // the Pokemon is still in.
                if matches!(ab, Ab::AirLock | Ab::CloudNine) {
                    self.each_event_draws();
                }
                let m = &self.sides[s].mons[oi];
                if m.vol.switch_copyvolatile {
                    baton = Some((m.boosts, m.vol));
                } else if m.vol.switch_shedtail {
                    let mut v = Vol::default();
                    v.substitute = m.vol.substitute;
                    baton = Some(([0; 7], v));
                }
            }
            let m = &mut self.sides[s].mons[oi];
            m.slot = -1;
            if !m.fainted {
                m.boosts = [0; 7];
                m.vol = Vol::default();
                // clearVolatile -> setSpecies: the raw Speed stat.
                m.speed = m.stats[5] as i32;
                // Types / ability revert on switch-out (megas stay mega);
                // a non-permanent battle forme reverts (Aegislash-Blade).
                m.types = m.base_types;
                m.ability = m.base_ability;
                m.revert_alt_forme();
            }
        }
        self.sides[s].active[p.i()] = to as u8;
        if old != crate::state::NO_MON {
            self.sides[s].swap_order(old, to as u8);
        }
        let m = &mut self.sides[s].mons[to];
        m.slot = p.slot as i8;
        m.vol = Vol::default();
        // Toxic's counter restarts on switch-in.
        if m.status == Status::Tox {
            m.status_turns = 0;
        }
        m.vol.newly_switched = true;
        m.reveal.seen = true;
        if let Some((boosts, v)) = baton {
            if v.substitute > 0 && v.switch_shedtail {
                m.vol.substitute = v.substitute;
            } else {
                m.boosts = boosts;
                m.vol.substitute = v.substitute;
                m.vol.confusion = v.confusion;
                m.vol.leech_seed = v.leech_seed;
                m.vol.leech_seed_src = v.leech_seed_src;
                m.vol.aqua_ring = v.aqua_ring;
                m.vol.ingrain = v.ingrain;
                m.vol.magnet_rise = v.magnet_rise;
                m.vol.crit_stage = v.crit_stage;
                m.vol.perish = v.perish;
                m.vol.curse = v.curse;
                m.vol.heal_block = v.heal_block;
                m.vol.no_retreat = v.no_retreat;
            }
        }
        // runSwitch's BeforeSwitchIn event (pinned illusion's onBeforeSwitchIn): the last
        // unfainted party member behind this slot; an active ally counts when it sits after
        // the slot.
        if self.dex.ab(self.m(p).ability) == Ab::Illusion {
            let pick = self.illusion_target(p.s(), p.i());
            self.mm(p).vol.illusion_species = pick;
        }
        if drag {
            blog!(self, "|drag|{}|{}|{}", self.name(p), self.details(p), self.hp_text(p));
        } else if from_move != 0 {
            blog!(self, "|switch|{}|{}|{}|[from] {}", self.name(p), self.details(p), self.hp_text(p), self.dex.mv(from_move).name);
        } else {
            blog!(self, "|switch|{}|{}|{}", self.name(p), self.details(p), self.hp_text(p));
        }
    }

    /// Pinned `Battle.swapPosition` (sim/battle.ts:1592) as Ally Switch uses it: the user and
    /// the other active Pokemon of its side trade slots. `active`, `Mon.slot` and the party order
    /// (`side.pokemon`) follow; the queued actions of both follow their Pokemon (Showdown's queue
    /// holds the objects). References the engine keeps as position codes are re-coded where
    /// pinned keeps the object (Attract and Partially Trapped: `effectState.source`) and left
    /// alone where pinned keeps the slot (Leech Seed `sourceSlot`, `attackedBy.slot`, the Wish
    /// and Healing Wish slot conditions: all resolve through getAtSlot). The two `Swap` events
    /// then settle Healing Wish for the new occupant of each slot, the displaced Pokemon first.
    pub fn swap_position(&mut self, p: Pos) -> bool {
        let s = p.s();
        let q = p.ally();
        let a = self.sides[s].active[p.i()];
        let b = self.sides[s].active[q.i()];
        if a == crate::state::NO_MON || b == crate::state::NO_MON || self.sides[s].mons[b as usize].fainted {
            return false;
        }
        blog!(self, "|swap|{}|{}|[from] move: Ally Switch", self.name(p), q.i());
        self.sides[s].active.swap(0, 1);
        self.sides[s].mons[a as usize].slot = q.slot as i8;
        self.sides[s].mons[b as usize].slot = p.slot as i8;
        self.sides[s].swap_order(a, b);
        let (ca, cb) = (p.code(), q.code());
        for c in 0..4u8 {
            let r = Pos::from_code(c);
            let i = self.sides[r.s()].active[r.i()];
            if i == crate::state::NO_MON {
                continue;
            }
            let v = &mut self.sides[r.s()].mons[i as usize].vol;
            if v.attract {
                if v.attract_src == ca && v.attract_mon == a {
                    v.attract_src = cb;
                } else if v.attract_src == cb && v.attract_mon == b {
                    v.attract_src = ca;
                }
            }
            if v.partial_trap > 0 {
                if v.partial_trap_src == ca && v.partial_trap_mon == a {
                    v.partial_trap_src = cb;
                } else if v.partial_trap_src == cb && v.partial_trap_mon == b {
                    v.partial_trap_src = ca;
                }
            }
        }
        self.queue.for_each_mut(|act| {
            if act.pos.s() == s {
                if act.mon == a {
                    act.pos = q;
                } else if act.mon == b {
                    act.pos = p;
                }
            }
        });
        // runEvent('Swap', target, pokemon), then runEvent('Swap', pokemon, target).
        self.slot_condition_swap(p);
        self.slot_condition_swap(q);
        true
    }

    /// Healing Wish's slot condition on a `Swap` event (pinned healingwish.condition.onSwap): the
    /// Pokemon now in the slot is healed and cured when it needs either. Wish has no onSwap.
    fn slot_condition_swap(&mut self, q: Pos) {
        if self.sides[q.s()].healing_wish[q.i()] && self.is_live(q) {
            let m = self.m(q);
            if m.hp < m.max_hp || m.status != Status::None {
                self.sides[q.s()].healing_wish[q.i()] = false;
                let mh = self.m(q).max_hp as u32;
                self.heal(q, mh);
                self.cure_status(q);
            }
        }
    }

    /// Pinned illusion's onBeforeSwitchIn: scan the party order from the end down to
    /// `slot + 1` and take the first unfainted member (0 when there is none). Only brought
    /// members are candidates: pinned scans `side.pokemon`, which holds the four brought ones,
    /// while `order[4..6]` holds the ones left behind (never fainted, so they must be skipped).
    pub fn illusion_target(&self, side: usize, slot: usize) -> crate::dex::SpeciesId {
        let s = &self.sides[side];
        for k in (slot + 1..6).rev() {
            let i = s.order[k] as usize;
            if s.mons[i].brought && !s.mons[i].fainted {
                return s.mons[i].species;
            }
        }
        0
    }

    /// Forced out (Red Card, Roar...): random replacement.
    pub fn drag_out(&mut self, p: Pos) {
        if !self.is_live(p) {
            return;
        }
        let s = p.s();
        // getRandomSwitchable: the unfainted brought Pokemon behind the actives, party order.
        let mut options = [0usize; 4];
        let mut n = 0;
        for &i in &self.sides[s].order[2..4] {
            if self.sides[s].can_switch_to(i as usize) {
                options[n] = i as usize;
                n += 1;
            }
        }
        if n == 0 {
            return;
        }
        let to = options[self.rng.below(n as u32) as usize];
        // switchIn(isDrag): the dragged-out Pokemon's queued actions are cancelled and
        // runSwitch happens at once.
        let old = self.sides[s].active[p.i()];
        self.queue.remove_where(|a| a.pos == p && a.mon == old);
        self.switch_out_in(p, to, true);
        self.run_switch_in(&[p]);
    }

    /// Run switch-in effects for Pokemon that just entered, in Showdown's
    /// order: handlers sorted by priority, then speed (ties resolved once).
    pub fn run_switch_in(&mut self, switchers: &[Pos]) {
        crate::prof!("run_switch_in");
        let mut live: crate::fixed::FixedVec<Pos, 4> = switchers.iter().copied().filter(|&p| self.is_live(p)).collect();
        if live.is_empty() {
            return;
        }
        // speedOrder: speedSort(getAllActive(true)) by cached speed, fainted included.
        let mut all: crate::fixed::FixedVec<(i32, Pos), 4> = (0..4u8)
            .map(Pos::from_code)
            .filter(|&q| self.sides[q.s()].active[q.i()] != crate::state::NO_MON)
            .map(|q| (self.m(q).speed, q))
            .collect();
        crate::rng::speed_sort(&mut self.rng, &mut all, |a, b| b.0.cmp(&a.0));
        live.sort_by_key(|&q| all.iter().position(|x| x.1 == q).unwrap_or(usize::MAX));

        // Priority > 0: Healing Wish (slot condition).
        for &p in &live {
            if self.sides[p.s()].healing_wish[p.i()] && self.is_live(p) {
                let m = self.m(p);
                if m.hp < m.max_hp || m.status != Status::None {
                    self.sides[p.s()].healing_wish[p.i()] = false;
                    let mh = self.m(p).max_hp as u32;
                    self.heal(p, mh);
                    self.cure_status(p);
                }
            }
        }
        // Priority 0, one switcher at a time in speedOrder: hazards (side conditions, subOrder
        // 4), the ability (7), the item (8). Seeds (priority -1) follow for everyone.
        for &p in &live {
            self.entry_hazards(p);
            self.process_faints();
            if self.is_live(p) {
                self.ability_start(p);
                self.process_faints();
            }
            if self.is_live(p) && self.it(p) == It::AirBalloon {
                self.item_start(p);
            }
        }
        for &p in &live {
            if self.is_live(p) && matches!(self.it(p), It::GrassySeed | It::PsychicSeed | It::ElectricSeed | It::MistySeed) {
                self.item_start(p);
            }
        }
        // White Herb (onAnySwitchIn, priority -2): one handler per active holder, in
        // speedOrder (the fractional speeds rule out ties).
        for &(_, q) in &all {
            self.check_white_herb(q);
        }
    }

    fn entry_hazards(&mut self, p: Pos) {
        if !self.is_live(p) {
            return;
        }
        let conds = self.sides[p.s()].conds;
        if conds.stealth_rock {
            let eff = self.effectiveness(Type::Rock, p, None);
            let mh = self.m(p).max_hp as u32;
            let dmg = match eff {
                2 => mh / 2,
                1 => mh / 4,
                0 => mh / 8,
                -1 => mh / 16,
                _ => mh / 32,
            };
            self.damage_from(p, dmg.max(1), None, DmgKind::Indirect, LogFrom::Cond("Stealth Rock"));
        }
        if conds.toxic_spikes > 0 && self.is_live(p) && self.is_grounded(p) {
            if self.m(p).has_type(Type::Poison) {
                self.sides[p.s()].conds.toxic_spikes = 0;
            } else {
                let st = if conds.toxic_spikes >= 2 { Status::Tox } else { Status::Psn };
                self.try_set_status(p, st, None);
            }
        }
        if conds.spikes > 0 && self.is_live(p) && self.is_grounded(p) {
            let mh = self.m(p).max_hp as u32;
            let dmg = match conds.spikes {
                1 => mh / 8,
                2 => mh / 6,
                _ => mh / 4,
            };
            self.damage_from(p, dmg, None, DmgKind::Indirect, LogFrom::Cond("Spikes"));
        }
        if conds.sticky_web && self.is_live(p) && self.is_grounded(p) {
            let mut d = [0i8; 7];
            d[B_SPE] = -1;
            let foe = p.foe(0);
            self.boost(p, &d, BoostSrc::Move(foe));
        }
    }

    /// An ability's onStart (switch-in or gained mid-battle).
    pub fn ability_start(&mut self, p: Pos) {
        if !self.is_live(p) {
            return;
        }
        let ab = self.ab(p);
        let mut fallen = 0;
        match ab {
            Ab::Intimidate => self.intimidate(p),
            Ab::ScreenCleaner => {
                let mut any = false;
                for side in 0..2 {
                    let c = &mut self.sides[side].conds;
                    if c.reflect > 0 || c.light_screen > 0 || c.aurora_veil > 0 {
                        any = true;
                    }
                    c.reflect = 0;
                    c.light_screen = 0;
                    c.aurora_veil = 0;
                }
                if any {
                    self.reveal_ability(p);
                }
            }
            Ab::Drizzle => {
                self.reveal_ability(p);
                self.set_weather(Weather::Rain, p, true);
            }
            Ab::Drought => {
                self.reveal_ability(p);
                self.set_weather(Weather::Sun, p, true);
            }
            Ab::SandStream => {
                self.reveal_ability(p);
                self.set_weather(Weather::Sand, p, true);
            }
            Ab::SnowWarning => {
                self.reveal_ability(p);
                self.set_weather(Weather::Snow, p, true);
            }
            Ab::GrassySurge => {
                self.reveal_ability(p);
                self.set_terrain(Terrain::Grassy, p, true);
            }
            Ab::PsychicSurge => {
                self.reveal_ability(p);
                self.set_terrain(Terrain::Psychic, p, true);
            }
            Ab::ElectricSurge => {
                self.reveal_ability(p);
                self.set_terrain(Terrain::Electric, p, true);
            }
            Ab::MistySurge => {
                self.reveal_ability(p);
                self.set_terrain(Terrain::Misty, p, true);
            }
            Ab::Hospitality => {
                if let Some(a) = self.live_ally(p) {
                    let amt = self.m(a).max_hp as u32 / 4;
                    if self.m(a).hp < self.m(a).max_hp {
                        self.reveal_ability(p);
                        // Pinned heal(ally.baseMaxhp / 4, ally, pokemon): the holder is `[of]`.
                        self.heal_from(a, amt, LogFrom::Ability("Hospitality"), Some(p));
                    }
                }
            }
            Ab::Trace => {
                // Copy a random opposing ability that can be copied.
                let foes: crate::fixed::FixedVec<Pos, 2> = self.live_foes(p).collect();
                let ok: crate::fixed::FixedVec<Pos, 2> = foes
                    .iter()
                    .copied()
                    .filter(|&q| {
                        let a = self.dex.ability(self.m(q).ability);
                        !a.cantsuppress && !NO_TRACE.contains(&a.id.as_str())
                    })
                    .collect();
                if !ok.is_empty() {
                    let q = ok[self.rng.below(ok.len() as u32) as usize];
                    let new = self.m(q).ability;
                    self.reveal_ability(p);
                    self.reveal_ability(q);
                    self.mm(p).ability = new;
                    self.mm(p).vol.ability_changed = true;
                    // Pinned trace onUpdate: `-ability|tracer|copied|Trace|[from] ability: Trace|[of] source`.
                    blog!(self, "|-ability|{}|{}|Trace|[from] ability: Trace|[of] {}", self.name(p), self.dex.ability(new).name, self.name(q));
                    self.ability_start(p);
                }
            }
            Ab::Frisk => {
                for &q in &self.live_foes(p).collect::<crate::fixed::FixedVec<_, 2>>() {
                    // Pinned frisk onStart.
                    let it = self.m(q).item;
                    if it != 0 {
                        let iname = self.dex.item(it).name.clone();
                        blog!(self, "|-item|{}|{}|[from] ability: Frisk|[of] {}", self.name(q), iname, self.name(p));
                    }
                    self.reveal_item(q);
                }
                self.reveal_ability(p);
            }
            Ab::SupremeOverlord => {
                fallen = self.sides[p.s()].total_fainted.min(5);
            }
            // Pinned onStart handlers that announce the ability.
            Ab::FairyAura => {
                self.reveal_ability(p);
                blog!(self, "|-ability|{}|Fairy Aura", self.name(p));
            }
            Ab::Unnerve => {
                self.reveal_ability(p);
                blog!(self, "|-ability|{}|Unnerve", self.name(p));
            }
            Ab::Pressure => {
                self.reveal_ability(p);
                blog!(self, "|-ability|{}|Pressure", self.name(p));
            }
            Ab::MoldBreaker => {
                self.reveal_ability(p);
                blog!(self, "|-ability|{}|Mold Breaker", self.name(p));
            }
            // onStart: eachEvent('WeatherChange') (also on its End).
            Ab::AirLock | Ab::CloudNine => {
                self.reveal_ability(p);
                // Pinned cloudnine/airlock onSwitchIn.
                let aname = if ab == Ab::AirLock { "Air Lock" } else { "Cloud Nine" };
                blog!(self, "|-ability|{}|{}", self.name(p), aname);
                self.each_event_draws();
            }
            Ab::CuriousMedicine => {
                if let Some(a) = self.live_ally(p) {
                    self.mm(a).boosts = [0; 7];
                }
            }
            _ => {}
        }
        if ab == Ab::SupremeOverlord {
            self.mm(p).vol.fallen = fallen;
            // Pinned onStart: the activation, then the silent fallen counter.
            if fallen > 0 {
                blog!(self, "|-activate|{}|ability: Supreme Overlord", self.name(p));
                blog!(self, "|-start|{}|fallen{}|[silent]", self.name(p), fallen);
            }
        }
    }

    /// An item's onStart at switch-in.
    pub fn item_start(&mut self, p: Pos) {
        match self.it(p) {
            It::AirBalloon => self.reveal_item(p),
            It::GrassySeed | It::PsychicSeed | It::ElectricSeed | It::MistySeed => self.check_seed(p),
            _ => {}
        }
    }
}
