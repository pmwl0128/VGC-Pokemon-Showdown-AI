//! Stats, speed, types and the damage formula, with Showdown's integer
//! arithmetic (4096-based modifiers, truncation at each step).

use super::ties::{Ev, Hk, Whitelist};
use super::Battle;
use crate::dex::{
    flag, Category, MoveId, Target, Terrain, Weather, ATK, B_ACC, B_EVA, DEF, SPA, SPD, SPE,
};
use crate::dex::{hook, Cond, Hooks, Status};
use crate::kinds::{Ab, It, MoveFx};
use crate::rng::Rng;
use crate::state::{Pos, SemiInv};
use crate::types::Type;

/// Showdown's `modify(value, numerator/denominator)`.
#[inline]
pub fn modify(value: u32, num: u32, den: u32) -> u32 {
    let m = (num as u64 * 4096 / den as u64) as u64;
    ((value as u64 * m + 2047) / 4096) as u32
}

/// A chain of 4096-based modifiers (Showdown's `chainModify`).
#[derive(Copy, Clone, Debug)]
pub struct Chain(pub u32);

impl Chain {
    pub fn new() -> Chain {
        Chain(4096)
    }
    #[inline]
    pub fn mul(&mut self, num: u32, den: u32) {
        let next = (num as u64 * 4096 / den as u64) as u64;
        self.0 = ((self.0 as u64 * next + 2048) >> 12) as u32;
    }
    #[inline]
    pub fn mul4096(&mut self, m: u32) {
        self.0 = ((self.0 as u64 * m as u64 + 2048) >> 12) as u32;
    }
    #[inline]
    pub fn apply(&self, value: u32) -> u32 {
        if self.0 == 4096 {
            return value;
        }
        ((value as u64 * self.0 as u64 + 2047) / 4096) as u32
    }
}

// Showdown effect-type subOrders (`Battle.resolvePriority`).
const SUB_MOVE: u8 = 0;
const SUB_COND: u8 = 2;
const SUB_SIDE: u8 = 4;
const SUB_FIELD: u8 = 5;
const SUB_ABIL: u8 = 7;
const SUB_ITEM: u8 = 8;

/// One collected event handler with Showdown's sort keys (`Battle.resolvePriority`): its
/// priority, its holder's cached speed (none for a side or the field), the effect-type
/// subOrder, the chainModify it applies (4096 when it leaves the value alone: Showdown
/// sorts every handler it finds, so those take part in ties and their draws too) and its
/// effect's rank (id order, ties.rs).
#[derive(Copy, Clone, Debug)]
struct Hm {
    prio: i16,
    holder: u8,
    speed: i32,
    sub: u8,
    m: u32,
    effect: u16,
}

/// Handler holders: 0..3 a Pokemon's position code, `SIDE + s` side s, `FIELD` the field.
const SIDE: u8 = 4;
const FIELD: u8 = 6;

#[inline]
fn holder_kind(h: u8) -> Hk {
    if h < SIDE {
        Hk::Pokemon
    } else if h < FIELD {
        Hk::Side
    } else {
        Hk::Field
    }
}

/// chainModify(num / den) as a 4096-based modifier.
#[inline]
fn fm(num: u32, den: u32) -> u32 {
    (num as u64 * 4096 / den as u64) as u32
}

const NO_MOD: u32 = 4096;

/// An event's handlers in findEventHandlers' collection order, chained in speedSort order
/// (priority desc, holder speed desc, subOrder asc). Ties shuffle with the battle RNG unless
/// the patched oracle's whitelist fixes them; the order matters because every chainModify
/// step rounds.
struct Handlers {
    /// Entries `..n` are initialized (left uninitialized beyond: no per-event clearing).
    v: [std::mem::MaybeUninit<Hm>; 16],
    n: usize,
}

impl Handlers {
    #[inline]
    fn new() -> Handlers {
        Handlers { v: [const { std::mem::MaybeUninit::uninit() }; 16], n: 0 }
    }
    #[inline]
    fn add(&mut self, prio: i16, holder: u8, sub: u8, m: u32, effect: u16) {
        debug_assert!(self.n < self.v.len(), "too many event handlers");
        if self.n < self.v.len() {
            self.v[self.n].write(Hm { prio, holder, speed: 0, sub, m, effect });
            self.n += 1;
        }
    }
    /// The collected handlers.
    #[inline]
    fn list(&mut self) -> &mut [Hm] {
        // SAFETY: `add` initializes every entry below `n` before counting it.
        unsafe { std::slice::from_raw_parts_mut(self.v.as_mut_ptr() as *mut Hm, self.n) }
    }
    /// The handler an item or ability defines for hook `k`, if it defines one.
    #[inline]
    fn hook(&mut self, hooks: &Hooks, k: usize, holder: u8, sub: u8, m: u32, effect: u16) {
        if hooks.has(k) {
            self.add(hooks.prio[k] as i16, holder, sub, m, effect);
        }
    }
    fn chain(&mut self, rng: &mut Rng, wl: &Whitelist, event: Ev) -> Chain {
        let n = self.n;
        let mut c = Chain::new();
        let v = self.list();
        if n > 1 {
            crate::rng::speed_sort_with(
                rng,
                v,
                |a, b| b.prio.cmp(&a.prio).then(b.speed.cmp(&a.speed)).then(a.sub.cmp(&b.sub)),
                |g| {
                    let kind = holder_kind(g[0].holder);
                    let artificial = if g[0].holder < SIDE {
                        g.iter().all(|h| h.holder == g[0].holder)
                    } else {
                        g.iter().all(|h| h.holder >= SIDE)
                    };
                    if !artificial || g.iter().any(|h| holder_kind(h.holder) != kind) {
                        return false;
                    }
                    if g.len() > Whitelist::MAX_EFFECTS {
                        return false;
                    }
                    let mut ids = [0u16; Whitelist::MAX_EFFECTS];
                    for (k, h) in g.iter().enumerate() {
                        ids[k] = h.effect;
                    }
                    wl.contains(event, &ids[..g.len()], kind)
                },
                |a, b| (a.effect, holder_kind(a.holder), a.holder).cmp(&(b.effect, holder_kind(b.holder), b.holder)),
            );
        }
        for h in v.iter() {
            if h.m != NO_MOD {
                c.mul4096(h.m);
            }
        }
        c
    }
}

const BOOST_NUM: [u32; 7] = [2, 3, 4, 5, 6, 7, 8];

/// Apply a stat stage to a stat (floor(stat * table)).
#[inline]
pub fn boosted(stat: u32, boost: i8) -> u32 {
    let b = boost.clamp(-6, 6);
    if b >= 0 {
        stat * BOOST_NUM[b as usize] / 2
    } else {
        stat * 2 / BOOST_NUM[(-b) as usize]
    }
}

/// One use of a move, with everything resolved at use time.
#[derive(Copy, Clone, Debug)]
pub struct ActiveMove {
    pub id: MoveId,
    pub fx: MoveFx,
    pub ty: Type,
    pub category: Category,
    pub base_power: u16,
    pub accuracy: u8,
    pub priority: i8,
    pub target: Target,
    pub flags: u32,
    pub crit_ratio: u8,
    pub will_crit: bool,
    pub spread_hit: bool,
    /// Pixilate & co. converted the type (1.2x).
    pub type_changer: bool,
    /// Current hit number (1-based) for multi-hit moves.
    pub hit: u8,
    pub sheer_force: bool,
    pub prankster: bool,
    pub infiltrates: bool,
    pub mold_breaker: bool,
    pub helping_hand: u8,
    pub mslot: u8,
    pub struggle: bool,
    pub confusion_hit: bool,
    /// Parental Bond second hit.
    pub parental_bond_hit: bool,
    /// Bypassed protection (Unseen Fist / Piercing Drill): damage x0.25.
    pub bypass_protect_quarter: bool,
    /// Showdown's move.smartTarget while true: getSmartTargets picked both foes (Dragon
    /// Darts); a hit step failing on one of them turns it off.
    pub smart_target: bool,
}

impl Battle {
    /// Chain an event's handlers in Showdown's order (holder speeds are only
    /// computed when there is more than one handler).
    fn chain_handlers(&mut self, h: &mut Handlers, event: Ev) -> Chain {
        crate::prof!("chain_handlers");
        if h.n > 1 {
            for e in h.list() {
                let c = e.holder;
                if c < 4 {
                    // resolvePriority: a Pokemon holder's handler takes its cached speed.
                    e.speed = self.m(Pos::from_code(c)).speed;
                }
            }
        }
        h.chain(&mut self.rng, &self.dex.whitelist, event)
    }

    // ---- abilities / items -----------------------------------------------

    /// Ability of the Pokemon at `p` (suppression aside).
    #[inline]
    pub fn ab(&self, p: Pos) -> Ab {
        self.dex.ab(self.m(p).ability)
    }

    /// Ability of a *target* of the current move: breakable abilities are
    /// ignored while a Mold Breaker user's move executes.
    #[inline]
    pub fn tab(&self, p: Pos) -> Ab {
        let id = self.m(p).ability;
        if self.mold_breaker && self.dex.ability(id).breakable {
            return Ab::Other;
        }
        self.dex.ab(id)
    }

    #[inline]
    pub fn it(&self, p: Pos) -> It {
        self.dex.it(self.m(p).item)
    }

    pub fn ability_on_field(&self, a: Ab) -> bool {
        let d = self.dex;
        for s in 0..2 {
            for slot in 0..2 {
                let i = self.sides[s].active[slot];
                if i != crate::state::NO_MON {
                    let m = &self.sides[s].mons[i as usize];
                    if m.alive() && d.ab(m.ability) == a {
                        return true;
                    }
                }
            }
        }
        false
    }

    // ---- field -------------------------------------------------------------

    /// Field weather, unless Cloud Nine / Air Lock is out.
    pub fn weather(&self) -> Weather {
        if self.field.weather == Weather::None {
            return Weather::None;
        }
        let d = self.dex;
        for c in 0..4 {
            let p = Pos::from_code(c);
            let i = self.sides[p.s()].active[p.i()];
            if i != crate::state::NO_MON {
                let m = &self.sides[p.s()].mons[i as usize];
                if m.alive() && matches!(d.ab(m.ability), Ab::CloudNine | Ab::AirLock) {
                    return Weather::None;
                }
            }
        }
        self.field.weather
    }

    /// Weather as seen by a move's user (Mega Sol counts as sun).
    pub fn user_weather(&self, p: Pos) -> Weather {
        if self.is_live(p) && self.ab(p) == Ab::MegaSol {
            return Weather::Sun;
        }
        self.weather()
    }

    pub fn is_grounded(&self, p: Pos) -> bool {
        let m = self.m(p);
        if self.field.gravity > 0 || m.vol.ingrain || m.vol.smacked_down {
            return true;
        }
        let item = self.it(p);
        if item == It::IronBall {
            return true;
        }
        if m.has_type(Type::Flying) && !m.vol.roost {
            return false;
        }
        if matches!(self.tab(p), Ab::Levitate | Ab::Eelevate) {
            return false;
        }
        if m.vol.magnet_rise > 0 {
            return false;
        }
        item != It::AirBalloon
    }

    pub fn terrain_for(&self, p: Pos) -> Terrain {
        if self.is_grounded(p) && self.m(p).vol.semi_inv == SemiInv::None {
            self.field.terrain
        } else {
            Terrain::None
        }
    }

    // ---- stats ----------------------------------------------------------------

    /// Boost stage after Unaware / Simple-type adjustments (ModifyBoost).
    #[inline]
    pub fn boost_of(&self, p: Pos, b: usize) -> i8 {
        self.m(p).boosts[b]
    }

    /// Speed with boosts and modifiers (getStat('spe')).
    pub fn speed(&self, p: Pos) -> u32 {
        let m = self.m(p);
        let mut spe = boosted(m.stats[SPE] as u32, m.boosts[SPE - 1]);
        let mut c = Chain::new();
        let ab = self.ab(p);
        match ab {
            Ab::SwiftSwim if self.weather() == Weather::Rain => c.mul(2, 1),
            Ab::Chlorophyll if self.weather() == Weather::Sun => c.mul(2, 1),
            Ab::SandRush if self.weather() == Weather::Sand => c.mul(2, 1),
            Ab::SlushRush if self.weather() == Weather::Snow => c.mul(2, 1),
            Ab::SurgeSurfer if self.field.terrain == Terrain::Electric => c.mul(2, 1),
            Ab::Unburden if m.vol.unburden && m.item == 0 => c.mul(2, 1),
            Ab::QuickFeet if m.status != Status::None => c.mul(3, 2),
            _ => {}
        }
        match self.it(p) {
            It::ChoiceScarf => c.mul(3, 2),
            It::IronBall => c.mul(1, 2),
            _ => {}
        }
        if self.sides[p.s()].conds.tailwind > 0 {
            c.mul(2, 1);
        }
        spe = c.apply(spe);
        // Paralysis (priority -101): finalModify, then halve.
        if m.status == Status::Par && ab != Ab::QuickFeet {
            spe = spe * 50 / 100;
        }
        spe.min(10000)
    }

    /// The tail of getActionSpeed. data/mods/champions/scripts.ts removes the Trick Room
    /// underflow: the Speed is negated, with no 13-bit truncation.
    #[inline]
    pub fn order_speed(&self, s: u32) -> i32 {
        if self.field.trick_room > 0 { -(s as i32) } else { s as i32 }
    }

    /// Showdown's `pokemon.getActionSpeed()`. A fainted Pokemon is inactive, so its ModifySpe
    /// event finds no handlers: the raw stat it was left with when it fainted.
    pub fn action_speed(&self, p: Pos) -> i32 {
        crate::prof!("action_speed");
        let m = self.m(p);
        if m.fainted {
            return self.order_speed(m.fainted_speed());
        }
        self.order_speed(self.speed(p))
    }

    // ---- types -------------------------------------------------------------

    pub fn types_of(&self, p: Pos) -> [Type; 2] {
        let m = self.m(p);
        let mut t = m.types;
        if m.vol.roost {
            // Roost removes Flying for the turn.
            if t[0] == Type::Flying {
                t[0] = if t[1] == Type::None { Type::Normal } else { t[1] };
                t[1] = Type::None;
            } else if t[1] == Type::Flying {
                t[1] = Type::None;
            }
        }
        t
    }

    /// Showdown's runEffectiveness (log2 sum over the target's types).
    pub fn effectiveness(&self, ty: Type, tgt: Pos, am: Option<&ActiveMove>) -> i32 {
        let chart = &self.dex.chart;
        // Disguise's onEffectiveness: a damaging move against an intact Mimikyu is neutral
        // whatever its type (an immune move never reaches the damage step; a Substitute in
        // front of the Mimikyu uses the chart as usual).
        if let Some(am) = am {
            if am.category != Category::Status
                && self.tab(tgt) == Ab::Disguise
                && self.disguise_intact(tgt)
                && !(self.m(tgt).vol.substitute > 0 && am.flags & flag::SOUND == 0 && !am.infiltrates && am.flags & flag::BYPASSSUB == 0)
            {
                return 0;
            }
        }
        // Iron Ball's onEffectiveness: a Ground move against a Flying type is
        // neutral for each of its types (not grounded by other means).
        if ty == Type::Ground && self.it(tgt) == It::IronBall && self.types_of(tgt).contains(&Type::Flying) {
            let m = self.m(tgt);
            if !(m.vol.ingrain || m.vol.smacked_down || self.field.gravity > 0) {
                return 0;
            }
        }
        let mut total = 0;
        for t in self.types_of(tgt) {
            if t == Type::None {
                continue;
            }
            let mut e = chart.log2(ty, t);
            if let Some(am) = am {
                if am.fx == MoveFx::FreezeDry && t == Type::Water {
                    e = 1;
                }
            }
            total += e;
        }
        if ty == Type::Fire && self.m(tgt).vol.tar_shot {
            total += 1;
        }
        total.clamp(-6, 6)
    }

    /// Type-chart (and grounding) immunity of `tgt` to a move of type `ty`.
    pub fn type_immune(&self, src: Pos, ty: Type, tgt: Pos, am: &ActiveMove) -> bool {
        if ty == Type::Ground {
            if !self.is_grounded(tgt) && !am.confusion_hit {
                return true;
            }
        }
        let chart = &self.dex.chart;
        for t in self.types_of(tgt) {
            if t == Type::None {
                continue;
            }
            if chart.immune(ty, t) {
                if t == Type::Ghost
                    && (ty == Type::Normal || ty == Type::Fighting)
                    && self.is_live(src)
                    && self.ab(src) == Ab::Scrappy
                {
                    continue;
                }
                if ty == Type::Ground && (self.field.gravity > 0 || self.m(tgt).vol.smacked_down || self.it(tgt) == It::IronBall || self.m(tgt).vol.ingrain) {
                    continue;
                }
                return true;
            }
        }
        false
    }

    // ---- moves: type and base power ---------------------------------------------

    /// Resolve the move's type (Weather Ball, Terrain Pulse, -ate abilities,
    /// Liquid Voice, Raging Bull...).
    pub fn resolve_move_type(&self, src: Pos, am: &mut ActiveMove) {
        let mv = self.dex.mv(am.id);
        match am.fx {
            MoveFx::WeatherBall => {
                am.ty = match self.user_weather(src) {
                    Weather::Rain => Type::Water,
                    Weather::Sun => Type::Fire,
                    Weather::Sand => Type::Rock,
                    Weather::Snow => Type::Ice,
                    Weather::None => Type::Normal,
                };
                if am.ty != Type::Normal {
                    am.base_power = mv.base_power * 2;
                }
            }
            MoveFx::TerrainPulse => {
                if self.is_grounded(src) {
                    am.ty = match self.field.terrain {
                        Terrain::Grassy => Type::Grass,
                        Terrain::Psychic => Type::Psychic,
                        Terrain::Electric => Type::Electric,
                        Terrain::Misty => Type::Fairy,
                        Terrain::None => Type::Normal,
                    };
                    if self.field.terrain != Terrain::None {
                        am.base_power = mv.base_power * 2;
                    }
                }
            }
            MoveFx::RagingBull => {
                // Tauros-Paldea formes use their breed's type; others stay Normal.
                am.ty = match self.dex.sp(self.m(src).species).id.as_str() {
                    "taurospaldeacombat" => Type::Fighting,
                    "taurospaldeablaze" => Type::Fire,
                    "taurospaldeaaqua" => Type::Water,
                    _ => am.ty,
                };
            }
            MoveFx::BurnUp | MoveFx::DoubleShock => {}
            _ => {}
        }
        if am.struggle || am.confusion_hit {
            return;
        }
        // -ate abilities (onModifyTypePriority -1, after the move's own change).
        let no_ate = matches!(am.fx, MoveFx::WeatherBall | MoveFx::TerrainPulse);
        let ab = self.ab(src);
        if am.ty == Type::Normal && !no_ate {
            let to = match ab {
                Ab::Pixilate => Some(Type::Fairy),
                Ab::Aerilate => Some(Type::Flying),
                Ab::Refrigerate => Some(Type::Ice),
                Ab::Galvanize => Some(Type::Electric),
                Ab::Dragonize => Some(Type::Dragon),
                _ => None,
            };
            if let Some(t) = to {
                am.ty = t;
                am.type_changer = true;
            }
        }
        if ab == Ab::LiquidVoice && am.flags & flag::SOUND != 0 {
            am.ty = Type::Water;
        }
    }

    /// basePowerCallback equivalents.
    pub fn base_power_callback(&self, src: Pos, tgt: Pos, am: &ActiveMove) -> u32 {
        let bp = am.base_power as u32;
        let s = self.m(src);
        let t = self.m(tgt);
        match am.fx {
            MoveFx::Eruption | MoveFx::WaterSpout | MoveFx::DragonEnergy => {
                (bp * s.hp as u32 / s.max_hp.max(1) as u32).max(1)
            }
            MoveFx::LastRespects => 50 + 50 * self.sides[src.s()].total_fainted.min(100) as u32,
            MoveFx::LowKick | MoveFx::GrassKnot => {
                let w = t.weight_hg;
                if w >= 2000 {
                    120
                } else if w >= 1000 {
                    100
                } else if w >= 500 {
                    80
                } else if w >= 250 {
                    60
                } else if w >= 100 {
                    40
                } else {
                    20
                }
            }
            MoveFx::HeavySlam | MoveFx::HeatCrash => {
                let ratio = s.weight_hg as f64 / t.weight_hg.max(1) as f64;
                if ratio >= 5.0 {
                    120
                } else if ratio >= 4.0 {
                    100
                } else if ratio >= 3.0 {
                    80
                } else if ratio >= 2.0 {
                    60
                } else {
                    40
                }
            }
            MoveFx::Acrobatics => {
                if s.item == 0 {
                    bp * 2
                } else {
                    bp
                }
            }
            MoveFx::StompingTantrum | MoveFx::TemperFlare => {
                if s.vol.last_move_failed {
                    bp * 2
                } else {
                    bp
                }
            }
            MoveFx::RageFist => (50 + 50 * s.vol.times_attacked as u32).min(350),
            MoveFx::Hex => {
                if t.status != Status::None {
                    bp * 2
                } else {
                    bp
                }
            }
            MoveFx::InfernalParade => {
                if t.status != Status::None {
                    bp * 2
                } else {
                    bp
                }
            }
            MoveFx::StoredPower | MoveFx::PowerTrip => {
                let pos: u32 = s.boosts.iter().filter(|&&b| b > 0).map(|&b| b as u32).sum();
                bp + 20 * pos
            }
            MoveFx::ElectroBall => {
                let ratio = self.speed(src) / self.speed(tgt).max(1);
                [40, 60, 80, 120, 150][(ratio as usize).min(4)]
            }
            MoveFx::HardPress => (100 * t.hp as u32 / t.max_hp.max(1) as u32).max(1),
            MoveFx::RisingVoltage => {
                if self.field.terrain == Terrain::Electric && self.is_grounded(tgt) {
                    bp * 2
                } else {
                    bp
                }
            }
            MoveFx::TripleAxel => 20 * am.hit as u32,
            MoveFx::BeatUp => {
                // 5 + base Attack / 10 of the hit-th party member.
                let side = &self.sides[src.s()];
                let (allies, n) = side.beat_up_allies(side.active[src.i()]);
                let k = (am.hit.max(1) as usize - 1).min(n.saturating_sub(1));
                // The set's species: a Mega counts as its base forme.
                let sp = side.mons[allies[k] as usize].base_species;
                5 + self.dex.sp(sp).base[1] as u32 / 10
            }
            MoveFx::Payback => {
                // Doubled unless the target just switched in or has yet to move.
                if t.vol.newly_switched || self.queue.will_move(tgt).is_some() {
                    bp
                } else {
                    bp * 2
                }
            }
            MoveFx::Assurance => {
                if t.vol.hurt_this_turn {
                    bp * 2
                } else {
                    bp
                }
            }
            MoveFx::Round => {
                if self.round_used {
                    bp * 2
                } else {
                    bp
                }
            }
            _ => bp,
        }
    }

    /// The BasePower event, after the crit roll: runEvent('BasePower', user, target, move)
    /// collects the move's own handler first, then the user's, the actives' onAlly / onAny,
    /// the target's onSource and the field's; every handler's modifier is chained in
    /// speedSort order.
    pub fn base_power_mods(&mut self, src: Pos, tgt: Pos, am: &ActiveMove, bp: u32) -> u32 {
        let d = self.dex;
        let s = self.m(src);
        let t = self.m(tgt);
        let sa = src.code();
        let sd = tgt.code();
        let mut h = Handlers::new();
        // The move's own onBasePower (unshifted; held by the user, subOrder 0).
        if d.mv(am.id).base_power_cb {
            let own = match am.fx {
                MoveFx::ExpandingForce if self.field.terrain == Terrain::Psychic && self.is_grounded(src) => fm(3, 2),
                MoveFx::KnockOff if t.item != 0 && !d.item(t.item).is_mega_stone => fm(3, 2),
                MoveFx::Facade if s.status != Status::None && s.status != Status::Slp => fm(2, 1),
                MoveFx::Venoshock | MoveFx::BarbBarrage if matches!(t.status, Status::Psn | Status::Tox) => fm(2, 1),
                MoveFx::LashOut if s.vol.stats_lowered_this_turn => fm(2, 1),
                MoveFx::SolarBeam | MoveFx::SolarBlade
                    if matches!(self.user_weather(src), Weather::Rain | Weather::Sand | Weather::Snow) =>
                {
                    fm(1, 2)
                }
                _ => NO_MOD,
            };
            h.add(0, sa, SUB_MOVE, own, d.mv(am.id).rank);
        }
        // The user's volatiles: Helping Hand (10, one chainModify of 1.5^n), Charge (9).
        if am.helping_hand > 0 {
            let mult = 1.5f64.powi(am.helping_hand as i32);
            h.add(10, sa, SUB_COND, (mult * 4096.0) as u32, d.cr(Cond::HelpingHand));
        }
        if s.vol.charge {
            h.add(9, sa, SUB_COND, if am.ty == Type::Electric { fm(2, 1) } else { NO_MOD }, d.cr(Cond::Charge));
        }
        // The user's ability and item.
        let ad = d.ability(s.ability);
        let m = match self.ab(src) {
            Ab::Technician if bp <= 60 => fm(3, 2),
            Ab::Rivalry if s.gender != 0 && t.gender != 0 => {
                if s.gender == t.gender {
                    fm(5, 4)
                } else {
                    fm(3, 4)
                }
            }
            Ab::IronFist if am.flags & flag::PUNCH != 0 => 4915,
            Ab::Reckless if d.mv(am.id).recoil.0 > 0 || d.mv(am.id).crash_damage => 4915,
            Ab::ToughClaws if am.flags & flag::CONTACT != 0 => 5325,
            Ab::SheerForce if am.sheer_force || d.mv(am.id).has_sheer_force_boost => 5325,
            Ab::Analytic if !self.any_other_will_move(src) => 5325,
            Ab::SandForce if self.weather() == Weather::Sand && matches!(am.ty, Type::Rock | Type::Ground | Type::Steel) => 5325,
            Ab::SupremeOverlord if s.vol.fallen > 0 => {
                const POW: [u32; 6] = [4096, 4506, 4915, 5325, 5734, 6144];
                POW[(s.vol.fallen as usize).min(5)]
            }
            Ab::Sharpness if am.flags & flag::SLICING != 0 => fm(3, 2),
            Ab::StrongJaw if am.flags & flag::BITE != 0 => fm(3, 2),
            Ab::MegaLauncher if am.flags & flag::PULSE != 0 => fm(3, 2),
            Ab::PunkRock if am.flags & flag::SOUND != 0 => 5325,
            // -ate abilities, only if they changed the type.
            _ if am.type_changer => 4915,
            _ => NO_MOD,
        };
        h.hook(&ad.hooks, hook::BASE_POWER, sa, SUB_ABIL, m, ad.rank);
        let id = d.item(s.item);
        let m = match self.it(src) {
            It::MuscleBand if am.category == Category::Physical => 4505,
            It::WiseGlasses if am.category == Category::Special => 4505,
            it if type_booster(it) == Some(am.ty) => 4915,
            _ => NO_MOD,
        };
        h.hook(&id.hooks, hook::BASE_POWER, sa, SUB_ITEM, m, id.rank);
        // alliesAndSelf then foes: onAlly / onAny. Fairy Aura boosts once however many
        // auras there are (move.auraBooster).
        let mut aura = am.ty == Type::Fairy && src != tgt && am.category != Category::Status;
        for q in [Pos::new(src.s(), 0), Pos::new(src.s(), 1), src.foe(0), src.foe(1)] {
            if !self.is_live(q) {
                continue;
            }
            let qd = d.ability(self.m(q).ability);
            if q.s() == src.s() {
                h.hook(&qd.hooks, hook::ALLY_BASE_POWER, q.code(), SUB_ABIL, NO_MOD, qd.rank);
            }
            let m = if aura && self.ab(q) == Ab::FairyAura {
                aura = false;
                5448
            } else {
                NO_MOD
            };
            h.hook(&qd.hooks, hook::ANY_BASE_POWER, q.code(), SUB_ABIL, m, qd.rank);
        }
        // The target's onSourceBasePower: Dry Skin (17).
        let td = d.ability(t.ability);
        let m = if am.ty == Type::Fire && self.tab(tgt) == Ab::DrySkin { fm(5, 4) } else { NO_MOD };
        h.hook(&td.hooks, hook::SOURCE_BASE_POWER, sd, SUB_ABIL, m, td.rank);
        // The terrain (field condition, priority 6).
        let terrain = match self.field.terrain {
            Terrain::Grassy => {
                let m = if matches!(d.mv(am.id).id.as_str(), "earthquake" | "bulldoze" | "magnitude")
                    && t.vol.semi_inv == SemiInv::None && self.is_grounded(tgt) {
                    fm(1, 2)
                } else if am.ty == Type::Grass && self.is_grounded(src) {
                    5325
                } else {
                    NO_MOD
                };
                Some((m, Cond::GrassyTerrain))
            }
            Terrain::Psychic => Some((if am.ty == Type::Psychic && s.vol.semi_inv == SemiInv::None && self.is_grounded(src) { 5325 } else { NO_MOD }, Cond::PsychicTerrain)),
            Terrain::Electric => Some((if am.ty == Type::Electric && s.vol.semi_inv == SemiInv::None && self.is_grounded(src) { 5325 } else { NO_MOD }, Cond::ElectricTerrain)),
            Terrain::Misty => Some((if am.ty == Type::Dragon && t.vol.semi_inv == SemiInv::None && self.is_grounded(tgt) { fm(1, 2) } else { NO_MOD }, Cond::MistyTerrain)),
            Terrain::None => None,
        };
        if let Some((m, id)) = terrain {
            h.add(6, FIELD, SUB_FIELD, m, d.cr(id));
        }
        self.chain_handlers(&mut h, Ev::BasePower).apply(bp).max(1)
    }

    /// Analytic: no other active Pokemon still has a move action this turn.
    fn any_other_will_move(&self, p: Pos) -> bool {
        (0..4).map(Pos::from_code).any(|q| q != p && self.is_live(q) && self.queue.will_move(q).is_some())
    }

    // ---- damage ----------------------------------------------------------------

    /// Disguise is still intact: the holder is a Mimikyu in its base forme that has not
    /// blocked a hit yet. Showdown's handlers test `target.species.id`, so a Disguise
    /// forced onto another species (the damage fixtures do that) does nothing.
    #[inline]
    pub(crate) fn disguise_intact(&self, p: Pos) -> bool {
        let m = self.m(p);
        !m.vol.disguise_busted
            && m.alt_species != 0
            && m.species != m.alt_species
            && self.dex.species_id("mimikyu") == Some(m.base_species)
    }

    /// The blocked hit still has to change the forme (Showdown's onUpdate).
    #[inline]
    pub(crate) fn disguise_bust_pending(&self, p: Pos) -> bool {
        let m = self.m(p);
        m.vol.disguise_busted
            && m.alt_species != 0
            && m.species != m.alt_species
            && self.dex.species_id("mimikyu") == Some(m.base_species)
    }

    /// Critical hit roll for one target.
    pub fn roll_crit(&mut self, src: Pos, tgt: Pos, am: &ActiveMove) -> bool {
        if am.confusion_hit {
            return false;
        }
        let s = self.m(src);
        let will = am.will_crit || s.vol.laser_focus > 0;
        let mut ratio = am.crit_ratio as i32 + s.vol.crit_stage as i32;
        if self.ab(src) == Ab::SuperLuck {
            ratio += 1;
        }
        if matches!(self.it(src), It::ScopeLens | It::RazorClaw) {
            ratio += 1;
        }
        if self.it(src) == It::Leek {
            let base = self.m(src).base_species;
            if Some(base) == self.dex.species_id("farfetchd") || Some(base) == self.dex.species_id("sirfetchd") {
                ratio += 2;
            }
        }
        let crit = if will {
            true
        } else if ratio <= 0 {
            false
        } else {
            let r = ratio.clamp(0, 4) as usize;
            let den = [0u32, 24, 8, 2, 1][r];
            self.rng.chance(1, den)
        };
        if crit && matches!(self.tab(tgt), Ab::BattleArmor | Ab::ShellArmor) {
            return false;
        }
        // Disguise's onCriticalHit: an intact Mimikyu cannot be hit critically. The roll
        // above already happened; a Substitute in front of it takes the hit as usual.
        if crit
            && self.tab(tgt) == Ab::Disguise
            && self.disguise_intact(tgt)
            && !(self.m(tgt).vol.substitute > 0 && tgt != src && am.flags & flag::SOUND == 0 && !am.infiltrates && am.flags & flag::BYPASSSUB == 0)
        {
            return false;
        }
        crit
    }

    /// Attack stat used by a move (getDamage's attacker side), after boosts
    /// and the ModifyAtk / ModifySpA event.
    fn attack_stat(&mut self, src: Pos, tgt: Pos, am: &ActiveMove, crit: bool) -> u32 {
        let mv = self.dex.mv(am.id);
        let physical = am.category == Category::Physical;
        let atk_holder = if mv.foul_play { tgt } else { src };
        let stat_idx = mv.override_offensive_stat.unwrap_or(if physical { ATK } else { SPA });
        let hm = self.m(atk_holder);
        let mut boost = hm.boosts[stat_idx - 1];
        // Unaware on the target ignores the attacker's boosts.
        if self.tab(tgt) == Ab::Unaware {
            boost = 0;
        }
        if crit && boost < 0 {
            boost = 0;
        }
        let mut atk = boosted(hm.stats[stat_idx] as u32, boost);
        // ModifyAtk (physical) / ModifySpA (special) on the user.
        let s = self.m(src);
        let ab = self.ab(src);
        let sa = src.code();
        let sd = tgt.code();
        // Hustle modifies the stat directly instead of chaining.
        if ab == Ab::Hustle && stat_idx == ATK {
            atk = modify(atk, 3, 2);
        }
        if stat_idx == ATK || stat_idx == SPA {
            // runEvent('ModifyAtk' / 'ModifySpA', user, target, move): the user's handlers, the
            // actives' onAlly / onAny, the target's onSource.
            let (event, own_k, ally_k, any_k, source_k) = if stat_idx == ATK {
                (Ev::ModifyAtk, hook::MODIFY_ATK, hook::ALLY_MODIFY_ATK, hook::ANY_MODIFY_ATK, hook::SOURCE_MODIFY_ATK)
            } else {
                (Ev::ModifySpA, hook::MODIFY_SPA, hook::ALLY_MODIFY_SPA, hook::ANY_MODIFY_SPA, hook::SOURCE_MODIFY_SPA)
            };
            let d = self.dex;
            let mut h = Handlers::new();
            // Flash Fire's volatile (priority 5).
            if s.vol.flash_fire {
                let m = if am.ty == Type::Fire && ab == Ab::FlashFire { fm(3, 2) } else { NO_MOD };
                h.add(5, sa, SUB_COND, m, d.cr(Cond::FlashFire));
            }
            let low_hp = (s.hp as u32) * 3 <= s.max_hp as u32;
            let m = match ab {
                Ab::HugePower | Ab::PurePower if physical => fm(2, 1),
                Ab::Guts if physical && s.status != Status::None => fm(3, 2),
                Ab::Blaze if am.ty == Type::Fire && low_hp => fm(3, 2),
                Ab::Torrent if am.ty == Type::Water && low_hp => fm(3, 2),
                Ab::Overgrow if am.ty == Type::Grass && low_hp => fm(3, 2),
                Ab::Swarm if am.ty == Type::Bug && low_hp => fm(3, 2),
                Ab::SolarPower if !physical && self.user_weather(src) == Weather::Sun => fm(3, 2),
                Ab::FireMane if am.ty == Type::Fire => fm(3, 2),
                Ab::WaterBubble if am.ty == Type::Water => fm(2, 1),
                _ => NO_MOD,
            };
            let ad = d.ability(s.ability);
            h.hook(&ad.hooks, own_k, sa, SUB_ABIL, m, ad.rank);
            let m = match self.it(src) {
                It::ChoiceBand if physical => fm(3, 2),
                It::ChoiceSpecs if !physical => fm(3, 2),
                It::LightBall if Some(self.m(src).base_species) == d.species_id("pikachu") => fm(2, 1),
                _ => NO_MOD,
            };
            let id = d.item(s.item);
            h.hook(&id.hooks, own_k, sa, SUB_ITEM, m, id.rank);
            for q in [Pos::new(src.s(), 0), Pos::new(src.s(), 1), src.foe(0), src.foe(1)] {
                if !self.is_live(q) {
                    continue;
                }
                let qd = d.ability(self.m(q).ability);
                if q.s() == src.s() {
                    h.hook(&qd.hooks, ally_k, q.code(), SUB_ABIL, NO_MOD, qd.rank);
                }
                h.hook(&qd.hooks, any_k, q.code(), SUB_ABIL, NO_MOD, qd.rank);
            }
            // The target's onSourceModifyAtk / onSourceModifySpA.
            let m = match self.tab(tgt) {
                Ab::ThickFat if matches!(am.ty, Type::Fire | Type::Ice) => fm(1, 2),
                Ab::Heatproof if am.ty == Type::Fire => fm(1, 2),
                Ab::WaterBubble if am.ty == Type::Fire => fm(1, 2),
                _ => NO_MOD,
            };
            let td = d.ability(self.m(tgt).ability);
            h.hook(&td.hooks, source_k, sd, SUB_ABIL, m, td.rank);
            atk = self.chain_handlers(&mut h, event).apply(atk);
        } else {
            // Body Press: runEvent('ModifyDef', user, ...).
            atk = self.def_mods(src, src, stat_idx, atk, false);
        }
        atk.max(1)
    }

    fn defense_stat(&mut self, src: Pos, tgt: Pos, am: &ActiveMove, crit: bool) -> u32 {
        let mv = self.dex.mv(am.id);
        let physical = am.category == Category::Physical;
        let stat_idx = mv.override_defensive_stat.unwrap_or(if physical { DEF } else { SPD });
        let t = self.m(tgt);
        let mut boost = t.boosts[stat_idx - 1];
        if mv.ignore_defensive || (self.is_live(src) && self.ab(src) == Ab::Unaware) {
            boost = 0;
        }
        if crit && boost > 0 {
            boost = 0;
        }
        let def = boosted(t.stats[stat_idx] as u32, boost);
        self.def_mods(tgt, src, stat_idx, def, true).max(1)
    }

    /// runEvent('ModifyDef' / 'ModifySpD') on `holder` during `src`'s move. Weather
    /// (priority 10) modifies the stat directly; during a Mega Sol user's move every
    /// Pokemon's effective weather is sun. `breakable`: the holder is the move's target.
    fn def_mods(&self, holder: Pos, src: Pos, stat_idx: usize, mut def: u32, breakable: bool) -> u32 {
        let t = self.m(holder);
        let w = if self.is_live(src) && self.ab(src) == Ab::MegaSol { Weather::Sun } else { self.weather() };
        if stat_idx == SPD && w == Weather::Sand && t.has_type(Type::Rock) {
            def = modify(def, 3, 2);
        }
        if stat_idx == DEF && w == Weather::Snow && t.has_type(Type::Ice) {
            def = modify(def, 3, 2);
        }
        let ab = if breakable { self.tab(holder) } else { self.ab(holder) };
        if stat_idx == DEF && ab == Ab::MarvelScale && t.status != Status::None {
            def = Chain(6144).apply(def);
        }
        def
    }

    /// Full damage for one hit (getDamage + modifyDamage). Returns None if the
    /// move does no damage (0 base power), Some(dmg) otherwise. `roll` fixes the random
    /// factor (0..16); None draws it where modifyDamage does, after the BasePower event.
    pub fn calc_damage(&mut self, src: Pos, tgt: Pos, am: &ActiveMove, crit: bool, roll: Option<u32>) -> Option<u32> {
        crate::prof!("calc_damage");
        let bp0 = self.base_power_callback(src, tgt, am);
        if bp0 == 0 {
            return None;
        }
        let bp = self.base_power_mods(src, tgt, am, bp0);
        let level = self.m(src).level as u32;
        let atk = self.attack_stat(src, tgt, am, crit);
        let def = self.defense_stat(src, tgt, am, crit);
        let base = ((2 * level / 5 + 2) * bp * atk / def) / 50;
        Some(self.modify_damage(base, src, tgt, am, crit, roll))
    }

    /// modifyDamage: spread, weather, crit, random, STAB, type, burn, then the
    /// ModifyDamage event (chained in Showdown's handler order).
    pub fn modify_damage(&mut self, base: u32, src: Pos, tgt: Pos, am: &ActiveMove, crit: bool, roll: Option<u32>) -> u32 {
        let mut dmg = base + 2;
        if am.spread_hit {
            dmg = modify(dmg, 3, 4);
        } else if am.parental_bond_hit {
            dmg = modify(dmg, 1, 4);
        }
        // Weather.
        let w = self.user_weather(src);
        match (w, am.ty) {
            (Weather::Rain, Type::Water) | (Weather::Sun, Type::Fire) => dmg = modify(dmg, 3, 2),
            (Weather::Rain, Type::Fire) | (Weather::Sun, Type::Water) => dmg = modify(dmg, 1, 2),
            _ => {}
        }
        if crit {
            dmg = dmg * 3 / 2;
        }
        // Random factor: 85..100.
        let roll = match roll {
            Some(r) => r,
            None => self.rng.below(16),
        };
        dmg = dmg * (100 - roll.min(15)) / 100;
        // STAB.
        if am.ty != Type::None && !am.confusion_hit {
            let s = self.m(src);
            if s.has_type(am.ty) {
                let stab = if self.ab(src) == Ab::Adaptability { (2, 1) } else { (3, 2) };
                dmg = modify(dmg, stab.0, stab.1);
            }
        }
        // Type effectiveness.
        let type_mod = if am.ty == Type::None { 0 } else { self.effectiveness(am.ty, tgt, Some(am)) };
        if type_mod > 0 {
            for _ in 0..type_mod {
                dmg *= 2;
            }
        } else if type_mod < 0 {
            for _ in 0..(-type_mod) {
                dmg /= 2;
            }
        }
        // Burn.
        let s = self.m(src);
        if s.status == Status::Brn && am.category == Category::Physical && self.ab(src) != Ab::Guts && am.fx != MoveFx::Facade {
            dmg = modify(dmg, 1, 2);
        }
        // runEvent('ModifyDamage', user, target, move): the user's handlers, the actives'
        // onAny (Friend Guard), the target's onSource, then both sides' screens (onAny).
        let d = self.dex;
        let t = self.m(tgt);
        let sa = src.code();
        let sd = tgt.code();
        let mut h = Handlers::new();
        let m = match self.ab(src) {
            Ab::Sniper if crit => fm(3, 2),
            Ab::TintedLens if type_mod < 0 => fm(2, 1),
            _ => NO_MOD,
        };
        let ad = d.ability(s.ability);
        h.hook(&ad.hooks, hook::MODIFY_DAMAGE, sa, SUB_ABIL, m, ad.rank);
        let m = match self.it(src) {
            It::LifeOrb => 5324,
            It::ExpertBelt if type_mod > 0 => 4915,
            _ => NO_MOD,
        };
        let id = d.item(s.item);
        h.hook(&id.hooks, hook::MODIFY_DAMAGE, sa, SUB_ITEM, m, id.rank);
        // Friend Guard (onAnyModifyDamage) guards the holder's allies.
        for q in [Pos::new(src.s(), 0), Pos::new(src.s(), 1), src.foe(0), src.foe(1)] {
            if !self.is_live(q) {
                continue;
            }
            let qd = d.ability(self.m(q).ability);
            if q.s() == src.s() {
                h.hook(&qd.hooks, hook::ALLY_MODIFY_DAMAGE, q.code(), SUB_ABIL, NO_MOD, qd.rank);
            }
            let m = if tgt != q && tgt.s() == q.s() && self.tab(q) == Ab::FriendGuard { fm(3, 4) } else { NO_MOD };
            h.hook(&qd.hooks, hook::ANY_MODIFY_DAMAGE, q.code(), SUB_ABIL, m, qd.rank);
        }
        // The target's onSourceModifyDamage: volatiles, ability, item.
        if t.vol.glaive_rush > 0 {
            h.add(0, sd, SUB_COND, fm(2, 1), d.cr(Cond::GlaiveRush));
        }
        // Minimize's onSourceModifyDamage: minimize-flagged moves deal double damage.
        if t.vol.minimize && am.flags & flag::MINIMIZE != 0 {
            h.add(0, sd, SUB_COND, fm(2, 1), d.cr(Cond::Minimize));
        }
        let mid = d.mv(am.id).id.as_str();
        match t.vol.semi_inv {
            SemiInv::Underground => {
                h.add(0, sd, SUB_COND, if matches!(mid, "earthquake" | "magnitude") { fm(2, 1) } else { NO_MOD }, d.cr(Cond::Dig))
            }
            SemiInv::Underwater => {
                h.add(0, sd, SUB_COND, if matches!(mid, "surf" | "whirlpool") { fm(2, 1) } else { NO_MOD }, d.cr(Cond::Dive))
            }
            SemiInv::Air if t.vol.charging > 0 && d.mv(t.moves[(t.vol.charging - 1) as usize]).id == "fly" => {
                h.add(0, sd, SUB_COND, NO_MOD, d.cr(Cond::Fly))
            }
            _ => {}
        }
        let m = match self.tab(tgt) {
            Ab::Multiscale if t.hp >= t.max_hp => fm(1, 2),
            Ab::Filter | Ab::SolidRock if type_mod > 0 => fm(3, 4),
            Ab::Fluffy => {
                let contact = am.flags & flag::CONTACT != 0;
                let fire = am.ty == Type::Fire;
                if contact && !fire {
                    fm(1, 2)
                } else if fire && !contact {
                    fm(2, 1)
                } else {
                    NO_MOD
                }
            }
            Ab::PunkRock if am.flags & flag::SOUND != 0 => fm(1, 2),
            Ab::AuraGuard if am.flags & flag::CONTACT != 0 => fm(1, 2),
            _ => NO_MOD,
        };
        let td = d.ability(t.ability);
        h.hook(&td.hooks, hook::SOURCE_MODIFY_DAMAGE, sd, SUB_ABIL, m, td.rank);
        let mut m = NO_MOD;
        if let Some(bt) = resist_berry(self.it(tgt)) {
            let hit_sub = t.vol.substitute > 0 && am.flags & flag::BYPASSSUB == 0 && !am.infiltrates;
            if bt == am.ty && (type_mod > 0 || bt == Type::Normal) && !hit_sub && !self.unnerved(tgt) {
                m = fm(1, 2);
            }
        }
        let tid = d.item(t.item);
        h.hook(&tid.hooks, hook::SOURCE_MODIFY_DAMAGE, sd, SUB_ITEM, m, tid.rank);
        // Reflect / Light Screen / Aurora Veil of either side (onAnyModifyDamage); only the
        // target side's applies, and Aurora Veil defers to the matching screen.
        let screens_apply = !crit && !am.infiltrates && src != tgt;
        for side in 0..2 {
            let c = &self.sides[side].conds;
            let own = side == tgt.s() && screens_apply;
            let holder = SIDE + side as u8;
            let phys = am.category == Category::Physical;
            let spec = am.category == Category::Special;
            if c.reflect > 0 {
                h.add(0, holder, SUB_SIDE, if own && phys { 2732 } else { NO_MOD }, d.cr(Cond::Reflect));
            }
            if c.light_screen > 0 {
                h.add(0, holder, SUB_SIDE, if own && spec { 2732 } else { NO_MOD }, d.cr(Cond::LightScreen));
            }
            if c.aurora_veil > 0 {
                let deferred = (phys && c.reflect > 0) || (spec && c.light_screen > 0);
                h.add(0, holder, SUB_SIDE, if own && (phys || spec) && !deferred { 2732 } else { NO_MOD }, d.cr(Cond::AuroraVeil));
            }
        }
        dmg = self.chain_handlers(&mut h, Ev::ModifyDamage).apply(dmg);
        if am.bypass_protect_quarter {
            dmg = modify(dmg, 1, 4);
        }
        if dmg == 0 {
            return 1;
        }
        dmg & 0xFFFF
    }

    /// Is `p` prevented from eating berries (Unnerve on the other side)?
    pub fn unnerved(&self, p: Pos) -> bool {
        self.live_foes(p).any(|q| self.ab(q) == Ab::Unnerve)
    }

    /// Accuracy check for one target. Returns true if the move hits.
    pub fn accuracy_hits(&mut self, src: Pos, tgt: Pos, am: &ActiveMove) -> bool {
        let mv = self.dex.mv(am.id);
        // Sheer Cold's level check and Ice-type immunity skip the whole step
        // (Showdown logs -immune [ohko] before the Accuracy event).
        if mv.ohko {
            let (sl, tl) = (self.m(src).level as u32, self.m(tgt).level as u32);
            if tl > sl || (mv.ohko_ice && self.m(tgt).has_type(Type::Ice)) {
                return false;
            }
        }
        if mv.target == Target::User && am.category == Category::Status && self.m(tgt).vol.semi_inv == SemiInv::None {
            return true;
        }
        // Toxic from a Poison-type user: accuracy = true, and hitStepAccuracy's
        // alwaysHit branch skips the Accuracy event.
        if mv.status == crate::dex::Status::Tox && am.category == Category::Status && self.m(src).has_type(Type::Poison) {
            return true;
        }
        // runEvent('Accuracy'): No Guard's onAnyAccuracy handlers in cached-speed order.
        // Equal speeds shuffle; this event has no other handlers in the pool.
        // Minimize's onAccuracy (the target's condition) is a handler of the same 'Accuracy'
        // event as No Guard's onAnyAccuracy: it joins the speed sort and makes the move hit.
        let minimized = self.m(tgt).vol.minimize && am.flags & flag::MINIMIZE != 0;
        let mut ng = [(0i32, 0u8); 4];
        let mut nn = 0;
        for c in 0..4u8 {
            let q = Pos::from_code(c);
            if self.active_unfainted(q) && (self.tab(q) == Ab::NoGuard || (minimized && q == tgt)) {
                ng[nn] = (self.m(q).speed, c);
                nn += 1;
            }
        }
        crate::rng::speed_sort(&mut self.rng, &mut ng[..nn], |a, b| b.0.cmp(&a.0));
        if self.ab(src) == Ab::NoGuard || self.tab(tgt) == Ab::NoGuard || minimized {
            return true;
        }
        if am.accuracy == 0 {
            return true;
        }
        if self.m(tgt).vol.glaive_rush > 0 {
            return true;
        }
        let mut acc = am.accuracy as u32;
        // Weather accuracy (onModifyMove).
        let w = self.user_weather(src);
        match am.fx {
            MoveFx::Hurricane | MoveFx::Thunder => match w {
                Weather::Rain => return true,
                Weather::Sun => acc = 50,
                _ => {}
            },
            MoveFx::Blizzard if w == Weather::Snow => return true,
            _ => {}
        }
        if mv.ohko {
            // (The level check / Ice immunity already returned above.)
            // Gen 7+: a non-Ice user's Sheer Cold is 20% accurate, 30 otherwise.
            let (sl, tl) = (self.m(src).level as u32, self.m(tgt).level as u32);
            let mut acc = 30;
            if mv.ohko_ice && !self.m(src).has_type(Type::Ice) {
                acc = 20;
            }
            acc += sl - tl;
            return self.rng.chance(acc.min(100), 100);
        }
        // ModifyAccuracy chain.
        let mut c = Chain::new();
        if self.field.gravity > 0 {
            c.mul(5, 3);
        }
        match self.ab(src) {
            Ab::CompoundEyes => c.mul4096(5325),
            Ab::Hustle if am.category == Category::Physical => c.mul4096(3277),
            _ => {}
        }
        match self.it(src) {
            It::WideLens => c.mul4096(4505),
            It::ZoomLens if self.m(tgt).vol.moved_this_turn => c.mul4096(4915),
            _ => {}
        }
        match self.tab(tgt) {
            Ab::SandVeil if self.weather() == Weather::Sand => c.mul4096(3277),
            Ab::SnowCloak if self.weather() == Weather::Snow => c.mul4096(3277),
            _ => {}
        }
        if self.it(tgt) == It::BrightPowder {
            c.mul4096(3686);
        }
        // Tangled Feet (onModifyAccuracy, priority -1): halved while the holder is confused.
        if self.tab(tgt) == Ab::TangledFeet && self.m(tgt).vol.confusion > 0 {
            c.mul4096(2048);
        }
        acc = c.apply(acc);
        // Accuracy / evasion stages.
        let mut boost: i32 = 0;
        if self.tab(tgt) != Ab::Unaware {
            boost = self.m(src).boosts[B_ACC] as i32;
        }
        if !mv.ignore_evasion && self.ab(src) != Ab::Unaware && !(self.ab(src) == Ab::Illuminate)
            && self.ab(src) != Ab::KeenEye
        {
            boost = (boost - self.m(tgt).boosts[B_EVA] as i32).clamp(-6, 6);
        }
        let boost = boost.clamp(-6, 6);
        if boost > 0 {
            acc = acc * (3 + boost as u32) / 3;
        } else if boost < 0 {
            acc = acc * 3 / (3 + (-boost) as u32);
        }
        self.rng.chance(acc.min(100), 100)
    }
}

/// Type-boosting held items.
pub fn type_booster(it: It) -> Option<Type> {
    Some(match it {
        It::MiracleSeed => Type::Grass,
        It::Charcoal => Type::Fire,
        It::MysticWater => Type::Water,
        It::BlackGlasses => Type::Dark,
        It::FairyFeather => Type::Fairy,
        It::SharpBeak => Type::Flying,
        It::NeverMeltIce => Type::Ice,
        It::TwistedSpoon => Type::Psychic,
        It::DragonFang => Type::Dragon,
        It::SpellTag => Type::Ghost,
        It::BlackBelt => Type::Fighting,
        It::MetalCoat => Type::Steel,
        It::HardStone => Type::Rock,
        It::SilkScarf => Type::Normal,
        It::Magnet => Type::Electric,
        It::PoisonBarb => Type::Poison,
        It::SoftSand => Type::Ground,
        It::SilverPowder => Type::Bug,
        _ => return None,
    })
}

/// Damage-halving berries.
pub fn resist_berry(it: It) -> Option<Type> {
    Some(match it {
        It::BabiriBerry => Type::Steel,
        It::ChartiBerry => Type::Rock,
        It::ChilanBerry => Type::Normal,
        It::ChopleBerry => Type::Fighting,
        It::CobaBerry => Type::Flying,
        It::ColburBerry => Type::Dark,
        It::HabanBerry => Type::Dragon,
        It::KasibBerry => Type::Ghost,
        It::KebiaBerry => Type::Poison,
        It::OccaBerry => Type::Fire,
        It::PasshoBerry => Type::Water,
        It::PayapaBerry => Type::Psychic,
        It::RindoBerry => Type::Grass,
        It::RoseliBerry => Type::Fairy,
        It::ShucaBerry => Type::Ground,
        It::TangaBerry => Type::Bug,
        It::WacanBerry => Type::Electric,
        It::YacheBerry => Type::Ice,
        _ => return None,
    })
}
