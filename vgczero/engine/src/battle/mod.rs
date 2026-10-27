//! The battle engine: requests, choices, the action queue and turn flow.
//!
//! Mechanics are split across submodules, all `impl Battle`:
//!   calc      stats, speed, types, effectiveness, damage formula
//!   moves     running a move (targets, hit steps, effects, scripted moves)
//!   effects   boosts, status, volatiles, healing, items, forme changes
//!   switching switching in/out and entry effects
//!   residual  end-of-turn effects
//!   legal     legal actions per slot

mod calc;
mod effects;
mod legal;
mod moves;
mod residual;
mod switching;
mod targeting;
mod turn;
pub mod ties;
pub use targeting::NO_ORIG;

pub use calc::ActiveMove;
pub use calc::{resist_berry as resist_berry_of, type_booster as type_booster_of};
pub use legal::is_choice as is_choice_item;

use crate::actions::{decode, preview_table, switch_action, Choice, SlotAction, PASS};
use crate::dex::dex;
use crate::rng::Rng;
use crate::state::{Field, Mon, Pos, Side, SideConds, NO_MON};
use crate::teams::TeamSpec;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    TeamPreview,
    /// Both sides choose moves for the turn.
    Move,
    /// Some slots must choose a replacement (after faints at end of turn, or
    /// mid-turn after U-turn / Parting Shot / Emergency Exit / Eject Button).
    Switch { midturn: bool },
    Ended,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RequestKind {
    /// Nothing to choose this step (the choice is ignored).
    Wait,
    TeamPreview,
    Move,
    Switch,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub kind: RequestKind,
    /// Slots that act in this request.
    pub slots: [bool; 2],
}

/// Final result: Some(0) / Some(1) side wins, Some(2) draw.
pub type Winner = Option<u8>;

#[derive(Copy, Clone, Debug)]
pub struct BattleConfig {
    /// Draw when this turn number is reached.
    pub turn_limit: u16,
    /// Allow choosing the ally as the target of damaging single-target moves.
    pub ally_damage_targeting: bool,
    /// Record a human-readable log.
    pub log: bool,
    /// Open team sheets for side 0 / side 1 (each seen by the opponent).
    pub sheets: [bool; 2],
}

impl Default for BattleConfig {
    fn default() -> Self {
        BattleConfig { turn_limit: 100, ally_damage_targeting: false, log: false, sheets: [false, false] }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ActionKind {
    Switch { to: u8 },
    Mega,
    /// `recharge`: Showdown's dex-less 'recharge' move (the turn after Hyper Beam), which
    /// stays that move even once its user has fainted.
    Move { mslot: u8, target: u8, recharge: bool },
    /// A switch chosen in a switch request (order 3).
    InstaSwitch { to: u8 },
    /// Revival Blessing's choice of a fainted Pokemon, chosen in a switch request (order 6).
    RevivalBlessing { to: u8 },
    /// Switch-in effects of the Pokemon at `pos` (order 101).
    RunSwitch,
    /// Battle start: the leads switch in (order 2).
    Start,
    /// eachEvent('BeforeTurn') (order 4).
    BeforeTurn,
    /// End-of-turn residual effects (order 300).
    Residual,
}

/// A queued action for one active slot.
#[derive(Copy, Clone, Debug)]
pub struct Action {
    pub kind: ActionKind,
    pub pos: Pos,
    /// Mon index that chose the action (actions are dropped if it left).
    pub mon: u8,
    /// Showdown action order: 103 switch, 104 mega, 200 move.
    pub order: u16,
    pub priority: i8,
    /// Fractional priority (Quick Claw +0.1, Lagging Tail / Quash -0.1) x10.
    pub frac: i8,
    pub speed: i32,
    /// The move this action will use (for Sucker Punch, Encore changes).
    pub move_id: u16,
    /// Showdown `action.targetLoc` after resolveAction (foe slot k = k + 1, own slot k = -(k + 1)).
    pub loc: i8,
    /// `action.originalTarget` as side * 6 + team index, or `NO_ORIG`.
    pub orig: u8,
}

pub const MAX_QUEUE: usize = 16;

/// Move action target for an action Encore rewrote: chosen at random on use.
pub const TARGET_RANDOM: u8 = 3;

#[derive(Copy, Clone, Debug)]
pub struct Queue {
    pub list: [Option<Action>; MAX_QUEUE],
    pub len: usize,
}

impl Default for Queue {
    fn default() -> Self {
        Queue { list: [None; MAX_QUEUE], len: 0 }
    }
}

impl Queue {
    pub fn push(&mut self, a: Action) {
        if self.len < MAX_QUEUE {
            self.list[self.len] = Some(a);
            self.len += 1;
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = &Action> {
        self.list[..self.len].iter().filter_map(|x| x.as_ref())
    }
    pub fn pop_front(&mut self) -> Option<Action> {
        if self.len == 0 {
            return None;
        }
        let a = self.list[0];
        for i in 1..self.len {
            self.list[i - 1] = self.list[i];
        }
        self.len -= 1;
        self.list[self.len] = None;
        a
    }
    pub fn remove_where(&mut self, f: impl Fn(&Action) -> bool) {
        let mut j = 0;
        for i in 0..self.len {
            if let Some(a) = self.list[i] {
                if !f(&a) {
                    self.list[j] = Some(a);
                    j += 1;
                }
            }
        }
        for k in j..self.len {
            self.list[k] = None;
        }
        self.len = j;
    }
    /// queue.willAct(): a move or switch is still to come (markers do not act).
    pub fn will_act(&self) -> bool {
        self.iter().any(|a| matches!(a.kind, ActionKind::Move { .. } | ActionKind::Switch { .. } | ActionKind::InstaSwitch { .. }))
    }
    /// Move action still to come for this position, if any.
    pub fn will_move(&self, p: Pos) -> Option<&Action> {
        self.iter().find(|a| a.pos == p && matches!(a.kind, ActionKind::Move { .. }))
    }
}

#[derive(Clone, Debug)]
pub struct Battle {
    /// Immutable process dex, resolved once when the battle is constructed.
    pub(crate) dex: &'static crate::dex::Dex,
    pub sides: [Side; 2],
    pub field: Field,
    pub turn: u16,
    pub rng: Rng,
    pub phase: Phase,
    pub queue: Queue,
    /// Slots that must choose a switch in the current Switch phase.
    pub switch_slots: [[bool; 2]; 2],
    pub winner: Winner,
    pub cfg: BattleConfig,
    /// Side whose Pokemon fainted last (Gen 5+ double-KO rule).
    pub last_faint_side: Option<u8>,
    /// Speed-ordered field positions for this switch-in event.
    pub log_lines: Vec<String>,
    /// The move currently executing ignores breakable abilities (Mold Breaker).
    pub(crate) mold_breaker: bool,
    /// Gen 9: a move already used this turn by this side (Round, Fusion...).
    pub(crate) round_used: bool,
    /// Number of turns where nothing happened (endless battle guard).
    pub stall_turns: u16,
    /// Showdown's midTurn: the beforeTurn and residual actions of this turn are queued.
    pub mid_turn: bool,
}

macro_rules! blog {
    ($b:expr, $($arg:tt)*) => {
        if $b.cfg.log { $b.log_lines.push(format!($($arg)*)); }
    };
}
pub(crate) use blog;

#[derive(Debug)]
pub struct ChoiceError(pub String);

impl std::fmt::Display for ChoiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Battle {
    /// A new battle at team preview.
    pub fn new(teams: [&TeamSpec; 2], seed: u64, cfg: BattleConfig) -> Battle {
        Battle::with_rng(teams, Rng::new(seed), cfg)
    }

    /// A new battle at team preview with an explicit stream (`Rng::sodium` reproduces a
    /// Showdown battle created with the same seed, including its construction draws).
    pub fn with_rng(teams: [&TeamSpec; 2], rng: Rng, cfg: BattleConfig) -> Battle {
        let mk_side = |t: &TeamSpec, sheet: bool| {
            let mut mons = [Mon::from_set(&t.mons[0], 0); 6];
            for (i, set) in t.mons.iter().enumerate() {
                mons[i] = Mon::from_set(set, i as u8);
            }
            Side {
                mons,
                active: [NO_MON; 2],
                conds: SideConds::default(),
                mega_used: false,
                total_fainted: 0,
                fainted_this_turn: false,
                fainted_last_turn: false,
                healing_wish: [false; 2],
                reviving: [false; 2],
                wish: [(0, 0); 2],
                sheet_open: sheet,
                order: [0, 1, 2, 3, 4, 5],
            }
        };
        let mut b = Battle {
            dex: dex(),
            sides: [mk_side(teams[0], cfg.sheets[0]), mk_side(teams[1], cfg.sheets[1])],
            field: Field::default(),
            turn: 0,
            rng,
            phase: Phase::TeamPreview,
            queue: Queue::default(),
            switch_slots: [[false; 2]; 2],
            winner: None,
            cfg,
            last_faint_side: None,
            log_lines: vec![],
            mold_breaker: false,
            round_used: false,
            stall_turns: 0,
            mid_turn: false,
        };
        // Pokemon are constructed p1 then p2, in team order; each unset gender is one
        // sample(['M', 'F']) draw.
        for s in 0..2 {
            for m in b.sides[s].mons.iter_mut() {
                if m.gender == crate::teams::GENDER_RANDOM {
                    m.gender = 1 + b.rng.below(2) as u8;
                }
            }
        }
        for s in 0..2 {
            if b.sides[s].sheet_open {
                for m in b.sides[s].mons.iter_mut() {
                    m.reveal.moves = (1u8 << m.n_moves) - 1;
                    m.reveal.ability = true;
                    m.reveal.item = true;
                }
            }
        }
        b
    }

    #[inline]
    pub fn ended(&self) -> bool {
        self.phase == Phase::Ended
    }

    // ---- accessors -------------------------------------------------------

    /// The Pokemon at a field position (must exist).
    #[inline]
    pub fn m(&self, p: Pos) -> &Mon {
        let i = self.sides[p.s()].active[p.i()];
        &self.sides[p.s()].mons[i as usize]
    }
    #[inline]
    pub fn mm(&mut self, p: Pos) -> &mut Mon {
        let i = self.sides[p.s()].active[p.i()];
        &mut self.sides[p.s()].mons[i as usize]
    }
    /// A living Pokemon is at this position.
    #[inline]
    pub fn is_live(&self, p: Pos) -> bool {
        let i = self.sides[p.s()].active[p.i()];
        i != NO_MON && self.sides[p.s()].mons[i as usize].alive()
    }
    /// Positions of all living active Pokemon (side 0 slot 0, 0/1, 1/0, 1/1).
    pub fn live_positions(&self) -> impl Iterator<Item = Pos> + '_ {
        (0..4).map(|c| Pos::from_code(c)).filter(move |&p| self.is_live(p))
    }
    pub fn live_foes(&self, p: Pos) -> impl Iterator<Item = Pos> + '_ {
        (0..2).map(move |i| p.foe(i)).filter(move |&q| self.is_live(q))
    }
    pub fn live_ally(&self, p: Pos) -> Option<Pos> {
        let a = p.ally();
        if self.is_live(a) {
            Some(a)
        } else {
            None
        }
    }

    // ---- requests ---------------------------------------------------------

    pub fn request(&self, side: usize) -> Request {
        match self.phase {
            Phase::TeamPreview => Request { kind: RequestKind::TeamPreview, slots: [false; 2] },
            Phase::Move => {
                let slots = [self.is_live(Pos::new(side, 0)), self.is_live(Pos::new(side, 1))];
                if slots[0] || slots[1] {
                    Request { kind: RequestKind::Move, slots }
                } else {
                    Request { kind: RequestKind::Wait, slots }
                }
            }
            Phase::Switch { .. } => {
                let slots = self.switch_slots[side];
                if slots[0] || slots[1] {
                    Request { kind: RequestKind::Switch, slots }
                } else {
                    Request { kind: RequestKind::Wait, slots }
                }
            }
            Phase::Ended => Request { kind: RequestKind::Wait, slots: [false; 2] },
        }
    }

    // ---- stepping --------------------------------------------------------

    /// Apply both sides' choices and run until the next decision point.
    pub fn step(&mut self, choices: [Choice; 2]) -> Result<(), ChoiceError> {
        crate::prof!("step");
        self.validate_choices(&choices)?;
        self.step_unchecked(choices);
        Ok(())
    }

    /// `step`'s validation alone: whether it would accept these choices (per-slot legality
    /// as `legal_mask` gives it, plus the joint rules: one switch target per side, one Mega).
    pub fn validate_choices(&self, choices: &[Choice; 2]) -> Result<(), ChoiceError> {
        match self.phase {
            Phase::Ended => Err(ChoiceError("battle is over".into())),
            Phase::TeamPreview => {
                for (s, c) in choices.iter().enumerate() {
                    if c.preview as usize >= preview_table().len() {
                        return Err(ChoiceError(format!("side {s}: bad preview {}", c.preview)));
                    }
                }
                Ok(())
            }
            Phase::Move => {
                for s in 0..2 {
                    self.validate_move_choice(s, &choices[s])?;
                }
                Ok(())
            }
            Phase::Switch { .. } => {
                for s in 0..2 {
                    self.validate_switch_choice(s, &choices[s])?;
                }
                Ok(())
            }
        }
    }

    /// `step` for choices that pass `validate_choices` (a policy drew them from `legal_mask`
    /// and the joint rules, or the caller validated them against this very state): the same
    /// transition without checking them again. Debug builds assert the precondition; a release
    /// build given an invalid choice makes an unspecified (but memory-safe) transition.
    pub fn step_unchecked(&mut self, choices: [Choice; 2]) {
        crate::prof!("step_unchecked");
        debug_assert!(self.validate_choices(&choices).is_ok(), "step_unchecked: choices fail validation");
        match self.phase {
            Phase::Ended => {}
            Phase::TeamPreview => {
                for (s, c) in choices.iter().enumerate() {
                    let row = preview_table()[c.preview as usize];
                    for &i in &row {
                        self.sides[s].mons[i as usize].brought = true;
                    }
                    self.sides[s].active = [row[0], row[1]];
                    let mut order = [row[0], row[1], row[2], row[3], 0, 0];
                    let mut k = 4;
                    for i in 0..6u8 {
                        if !row.contains(&i) {
                            order[k] = i;
                            k += 1;
                        }
                    }
                    self.sides[s].order = order;
                    for slot in 0..2 {
                        let mi = row[slot] as usize;
                        self.sides[s].mons[mi].slot = slot as i8;
                        self.sides[s].mons[mi].reveal.seen = true;
                    }
                }
                blog!(self, "|start");
                // commitChoices sorts the team actions (order 1, priority -index, speed =
                // getActionSpeed: the raw Speed stat before the battle starts): same-index
                // picks of equal speed tie and draw. Their order has no further effect here.
                let mut team = [(0i32, 0i32); 8];
                for (s, c) in choices.iter().enumerate() {
                    let row = preview_table()[c.preview as usize];
                    for (index, &i) in row.iter().enumerate() {
                        team[s * 4 + index] = (-(index as i32), self.sides[s].mons[i as usize].stats[5] as i32);
                    }
                }
                crate::rng::speed_sort(&mut self.rng, &mut team, |a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
                // start(): the start action is queued and midTurn set; endTurn makes turn 1.
                self.turn = 0;
                self.update_speed();
                self.queue = Queue::default();
                self.queue.push(Self::marker(ActionKind::Start, turn::ORDER_START));
                self.mid_turn = true;
                self.phase = Phase::Move;
                self.turn_loop();
            }
            Phase::Move => {
                self.commit_moves(&choices);
            }
            Phase::Switch { .. } => {
                let mut choices = choices;
                for s in 0..2 {
                    self.complete_switch_choice(s, &mut choices[s]);
                }
                // A Revival Blessing user that passes keeps its switch flag
                // (Showdown asks again after the next action).
                let mut revive_pass = [[false; 2]; 2];
                for s in 0..2 {
                    for slot in 0..2 {
                        if !self.switch_slots[s][slot] {
                            continue;
                        }
                        let p = Pos::new(s, slot);
                        if self.sides[s].reviving[slot] && decode(choices[s].slots[slot]) == SlotAction::Pass && self.is_live(p) {
                            revive_pass[s][slot] = true;
                            self.mm(p).vol.switch_flag = false;
                        }
                    }
                }
                self.commit_switches(&choices, revive_pass);
            }
        }
    }

    fn validate_switch_choice(&self, s: usize, c: &Choice) -> Result<(), ChoiceError> {
        let mut used = [false; 6];
        for slot in 0..2 {
            if !self.switch_slots[s][slot] {
                continue;
            }
            match decode(c.slots[slot]) {
                SlotAction::Switch(to) => {
                    let to = to as usize;
                    if !self.switch_target_ok(s, slot, to) || used[to] {
                        return Err(ChoiceError(format!("side {s} slot {slot}: cannot switch to {to}")));
                    }
                    used[to] = true;
                }
                SlotAction::Pass if self.switch_passes_allowed(s) => {}
                _ => return Err(ChoiceError(format!("side {s} slot {slot}: expected a switch"))),
            }
        }
        Ok(())
    }

    /// Showdown requires as many replacements as possible (forcedSwitchesLeft):
    /// with two flagged slots and one benched Pokemon, exactly one slot may
    /// pass. Per-slot legal masks cannot express that, so a choice passing on
    /// too many slots is completed here: the first passing flagged slot takes
    /// the first available Pokemon (instead of rejecting the choice).
    fn complete_switch_choice(&self, s: usize, c: &mut Choice) {
        let forced = self.forced_switches(s);
        let mut used = [false; 6];
        let mut switches = 0;
        for slot in 0..2 {
            if let (true, SlotAction::Switch(t)) = (self.switch_slots[s][slot], decode(c.slots[slot])) {
                used[t as usize] = true;
                switches += 1;
            }
        }
        for slot in 0..2 {
            if switches >= forced {
                break;
            }
            if self.switch_slots[s][slot] && decode(c.slots[slot]) == SlotAction::Pass {
                if let Some(i) = (0..6).find(|&i| self.switch_target_ok(s, slot, i) && !used[i]) {
                    used[i] = true;
                    c.slots[slot] = switch_action(i);
                    switches += 1;
                }
            }
        }
    }

    /// A switch request's target for a flagged slot: a healthy benched
    /// Pokemon, or a fainted one when the slot used Revival Blessing.
    pub fn switch_target_ok(&self, s: usize, slot: usize, i: usize) -> bool {
        if self.sides[s].reviving[slot] && matches!(self.phase, Phase::Switch { .. }) {
            let m = &self.sides[s].mons[i];
            m.brought && m.fainted
        } else {
            self.sides[s].can_switch_to(i)
        }
    }

    /// Revival Blessing's action: the fainted Pokemon `to` comes back with
    /// half its HP; if it is still in an active slot (a fainted ally), it
    /// switches back in there (instaswitch) and its queued action is dropped.
    /// Returns that slot, for the switch-in effects.
    pub(crate) fn revive(&mut self, p: Pos, to: usize) -> Option<Pos> {
        let s = p.s();
        self.sides[s].reviving[p.i()] = false;
        if self.is_live(p) {
            self.mm(p).vol.switch_flag = false;
        }
        {
            let m = &mut self.sides[s].mons[to];
            m.fainted = false;
            m.status = crate::dex::Status::None;
            m.status_turns = 0;
            m.hp = (m.max_hp / 2).max(1);
        }
        blog!(
            self,
            "|-heal|p{}: {}|{}/{}|[from] move: Revival Blessing",
            s + 1,
            self.mon_name(s, to),
            self.sides[s].mons[to].hp,
            self.sides[s].mons[to].max_hp
        );
        for slot in 0..2 {
            if self.sides[s].active[slot] as usize == to {
                let q = Pos::new(s, slot);
                self.queue.remove_where(|a| a.pos == q);
                self.switch_out_in(q, to);
                return Some(q);
            }
        }
        None
    }

    /// In a switch phase: how many of the side's flagged slots must switch
    /// (the rest pass, when there are fewer replacements than slots).
    pub fn forced_switches(&self, s: usize) -> usize {
        let flagged = self.switch_slots[s].iter().filter(|&&x| x).count();
        flagged.min(self.sides[s].bench_available() as usize)
    }

    /// A flagged slot may pass (Showdown's forcedPassesLeft > 0).
    pub fn switch_passes_allowed(&self, s: usize) -> bool {
        let flagged = self.switch_slots[s].iter().filter(|&&x| x).count();
        flagged > self.sides[s].bench_available() as usize
    }

    fn validate_move_choice(&self, s: usize, c: &Choice) -> Result<(), ChoiceError> {
        let mut used = [false; 6];
        let mut megas = 0;
        for slot in 0..2 {
            let p = Pos::new(s, slot);
            let a = c.slots[slot];
            if !self.action_legal(s, slot, a) {
                let mask = self.legal_mask(s, slot);
                return Err(ChoiceError(format!(
                    "side {s} slot {slot}: illegal action {a} ({:?}); legal mask {mask:#b}",
                    decode(a)
                )));
            }
            match decode(a) {
                SlotAction::Switch(to) => {
                    if used[to as usize] {
                        return Err(ChoiceError(format!("side {s}: both slots switch to {to}")));
                    }
                    used[to as usize] = true;
                }
                SlotAction::Move { mega: true, .. } => {
                    megas += 1;
                    let _ = p;
                }
                _ => {}
            }
        }
        if megas > 1 {
            return Err(ChoiceError(format!("side {s}: two Mega Evolutions")));
        }
        Ok(())
    }

    // ---- turn flow ---------------------------------------------------------

    /// Start-of-turn bookkeeping, then wait for move choices.
    pub(crate) fn begin_turn(&mut self) {
        crate::prof!("begin_turn");
        // endTurn's per-Pokemon DisableMove / TrapPokemon events.
        self.end_turn_event_sorts();
        if self.turn >= self.cfg.turn_limit {
            blog!(self, "|tie|turn limit");
            self.winner = Some(2);
            self.phase = Phase::Ended;
            return;
        }
        blog!(self, "|turn|{}", self.turn);
        for s in 0..2 {
            self.sides[s].fainted_last_turn = self.sides[s].fainted_this_turn;
            self.sides[s].fainted_this_turn = false;
            // Only brought Pokemon ever set these.
            for m in self.sides[s].mons.iter_mut().filter(|m| m.brought) {
                let v = &mut m.vol;
                v.last_move_failed = v.this_move_failed;
                v.this_move_failed = false;
                v.moved_this_turn = false;
                v.hurt_this_turn = false;
                v.stats_raised_this_turn = false;
                v.stats_lowered_this_turn = false;
                v.newly_switched = false;
                v.last_damage_taken = 0;
                v.stall_used_this_turn = false;
            }
        }
        self.round_used = false;
        self.phase = Phase::Move;
    }

    pub(crate) fn build_queue(&mut self, choices: &[Choice; 2]) {
        crate::prof!("build_queue");
        self.queue = Queue::default();
        let d = self.dex;
        for s in 0..2 {
            for slot in 0..2 {
                let p = Pos::new(s, slot);
                if !self.is_live(p) {
                    continue;
                }
                let mon_idx = self.sides[s].active[slot];
                match decode(choices[s].slots[slot]) {
                    SlotAction::Pass => {}
                    SlotAction::Switch(to) => {
                        self.queue.push(Action {
                            kind: ActionKind::Switch { to },
                            pos: p,
                            mon: mon_idx,
                            order: 103,
                            priority: 0,
                            frac: 0,
                            speed: 0,
                            move_id: 0,
                            loc: 0,
                            orig: NO_ORIG,
                        });
                    }
                    SlotAction::Move { slot: ms, target, mega } => {
                        if mega {
                            self.queue.push(Action {
                                kind: ActionKind::Mega,
                                pos: p,
                                mon: mon_idx,
                                order: 104,
                                priority: 0,
                                frac: 0,
                                speed: 0,
                                move_id: 0,
                                loc: 0,
                                orig: NO_ORIG,
                            });
                        }
                        let (mid, ms) = self.resolve_chosen_move(p, ms as usize);
                        let mv = d.mv(mid);
                        let recharge = self.m(p).vol.must_recharge;
                        let mut a = Action {
                            kind: ActionKind::Move { mslot: ms as u8, target, recharge },
                            pos: p,
                            mon: mon_idx,
                            order: 200,
                            priority: mv.priority,
                            frac: 0,
                            speed: 0,
                            move_id: mid,
                            loc: 0,
                            orig: NO_ORIG,
                        };
                        a.priority = self.modify_priority(p, mid, a.priority);
                        // A recharge turn is Showdown's 'recharge' move: priority 0, target self.
                        if recharge {
                            a.priority = 0;
                        }
                        // resolveAction: FractionalPriority (Quick Claw) first, then the target.
                        if self.m(p).item != 0 && d.it(self.m(p).item) == crate::kinds::It::QuickClaw && self.rng.chance(1, 5) {
                            a.frac = 1;
                            blog!(self, "|-activate|{}|item: Quick Claw", self.name(p));
                        }
                        if recharge {
                            // The locked 'recharge' move keeps lastMoveTargetLoc (no resolution
                            // draw); its getActionSpeed getTarget draws (see recharge_target_draw).
                            self.recharge_target_draw(p);
                        } else {
                            let (loc, orig) = self.resolve_target(p, mid, if self.m(p).vol.charging > 0 { None } else { Some(target) });
                            a.loc = loc;
                            a.orig = orig;
                            // getActionSpeed (end of resolveAction) calls getTarget for ModifyPriority.
                            let _ = self.get_target(p, mid, loc, NO_ORIG);
                        }
                        self.queue.push(a);
                    }
                }
            }
        }
        // commitChoices: speeds were computed at resolution; queue.sort() only.
        self.set_queue_speeds();
        self.sort_queue();
        let _ = PASS;
    }

    /// The move a slot will actually use (charging / locked / Struggle).
    pub(crate) fn resolve_chosen_move(&self, p: Pos, ms: usize) -> (u16, usize) {
        let m = self.m(p);
        if m.vol.charging > 0 {
            let s = (m.vol.charging - 1) as usize;
            return (m.moves[s], s);
        }
        if self.must_struggle(p) {
            return (self.dex.struggle, 0);
        }
        if m.vol.encore > 0 {
            let s = m.vol.encore_slot as usize;
            return (m.moves[s], s);
        }
        (m.moves[ms.min(m.n_moves.saturating_sub(1) as usize)], ms)
    }

    /// Showdown's getActionSpeed for every queued action: the speed, and for
    /// moves the priority (ModifyPriority: Grassy Glide, Prankster...).
    pub(crate) fn update_queue_speeds(&mut self) {
        crate::prof!("update_queue_speeds");
        // Showdown's getActionSpeed calls getTarget (Grassy Glide) for every queued move,
        // including moves of Pokemon that fainted since: random targets draw each time.
        for i in 0..self.queue.len {
            if let Some(a) = self.queue.list[i] {
                if let ActionKind::Move { .. } = a.kind {
                    if self.is_recharge(&a) {
                        self.recharge_target_draw(a.pos);
                    } else {
                        let _ = self.get_target(a.pos, a.move_id, a.loc, NO_ORIG);
                    }
                }
            }
        }
        self.set_queue_speeds();
    }

    /// getTarget for Showdown's 'recharge' move: it is not in the dex, so its target is
    /// undefined and getTarget always falls back to randomFoe().
    pub(crate) fn recharge_target_draw(&mut self, p: Pos) {
        let _ = self.get_random_target(p, crate::dex::Target::Normal);
    }

    /// The queued move is the user's recharge turn (Showdown's 'recharge' move).
    fn is_recharge(&self, a: &Action) -> bool {
        matches!(a.kind, ActionKind::Move { recharge: true, .. })
    }

    /// Speeds and priorities of the queued actions (no draws), right after updateSpeed. The
    /// action of a Pokemon that fainted since stays queued and is re-measured too: it is
    /// inactive, so no Pokemon handler (Prankster, Choice Scarf, Tailwind...) applies, only
    /// the move's own Grassy Glide check with item and ability ignored.
    fn set_queue_speeds(&mut self) {
        for i in 0..self.queue.len {
            if let Some(mut a) = self.queue.list[i] {
                if a.mon != NO_MON && self.sides[a.pos.s()].active[a.pos.i()] == a.mon {
                    // Callers run updateSpeed first and change nothing speed-related in
                    // between, so an unfainted Pokemon's cached speed is its action speed.
                    a.speed = if self.m(a.pos).fainted {
                        self.action_speed(a.pos)
                    } else {
                        debug_assert_eq!(self.m(a.pos).speed, self.action_speed(a.pos));
                        self.m(a.pos).speed
                    };
                    if let ActionKind::Move { .. } = a.kind {
                        let base = self.dex.mv(a.move_id).priority;
                        a.priority = if self.is_recharge(&a) {
                            0
                        } else if self.m(a.pos).fainted {
                            let m = self.m(a.pos);
                            let grounded = self.field.gravity > 0 || !m.has_type(crate::types::Type::Flying);
                            let glide = self.dex.mv(a.move_id).fx == crate::kinds::MoveFx::GrassyGlide
                                && self.field.terrain == crate::dex::Terrain::Grassy
                                && grounded;
                            base + glide as i8
                        } else {
                            self.modify_priority(a.pos, a.move_id, base)
                        };
                    }
                }
                self.queue.list[i] = Some(a);
            }
        }
    }

    /// Showdown's queue.changeAction(pokemon, {choice: 'move', moveid}) as Champions Encore
    /// uses it: cancelAction (every queued action of the Pokemon) then insertChoice, whose
    /// resolveAction rolls FractionalPriority, picks a random target and runs getActionSpeed.
    pub(crate) fn change_action(&mut self, p: Pos, mslot: usize, move_id: u16) {
        let mon = self.sides[p.s()].active[p.i()];
        // insertChoice updates the Pokemon's cached speed first.
        let s = self.action_speed(p);
        self.mm(p).speed = s;
        self.queue.remove_where(|a| a.pos == p && a.mon == mon);
        let d = self.dex;
        let mut a = Action {
            kind: ActionKind::Move { mslot: mslot as u8, target: TARGET_RANDOM, recharge: false },
            pos: p,
            mon,
            order: 200,
            priority: self.modify_priority(p, move_id, d.mv(move_id).priority),
            frac: 0,
            speed: 0,
            move_id,
            loc: 0,
            orig: NO_ORIG,
        };
        if self.m(p).item != 0 && d.it(self.m(p).item) == crate::kinds::It::QuickClaw && self.rng.chance(1, 5) {
            a.frac = 1;
            blog!(self, "|-activate|{}|item: Quick Claw", self.name(p));
        }
        let (loc, orig) = self.resolve_target(p, move_id, Some(TARGET_RANDOM));
        a.loc = loc;
        a.orig = orig;
        let _ = self.get_target(p, move_id, loc, NO_ORIG);
        a.speed = self.action_speed(p);
        self.insert_choice(a);
    }

    /// Showdown's queue.insertChoice for one resolved action: before the first queued action
    /// it sorts strictly ahead of; among equal-key actions at a random position.
    pub(crate) fn insert_choice(&mut self, a: Action) {
        #[inline]
        fn key(x: &Action) -> (i64, i64, i64, i64) {
            (x.order as i64, -(x.priority as i64), -(x.frac as i64), -(x.speed as i64))
        }
        let (mut first, mut last) = (None, None);
        for i in 0..self.queue.len {
            let Some(cur) = self.queue.list[i] else { continue };
            let c = key(&a).cmp(&key(&cur));
            if c != std::cmp::Ordering::Greater && first.is_none() {
                first = Some(i);
            }
            if c == std::cmp::Ordering::Less {
                last = Some(i);
                break;
            }
        }
        let index = match first {
            None => self.queue.len,
            Some(f) => {
                let l = last.unwrap_or(self.queue.len);
                if f == l { f } else { self.rng.range(f as u32, l as u32 + 1) as usize }
            }
        };
        if self.queue.len >= MAX_QUEUE {
            return;
        }
        for j in (index..self.queue.len).rev() {
            self.queue.list[j + 1] = self.queue.list[j];
        }
        self.queue.list[index] = Some(a);
        self.queue.len += 1;
    }

    /// Showdown's queue.prioritizeAction: the action goes next (order 3).
    pub(crate) fn prioritize_action(&mut self, i: usize) {
        if let Some(mut a) = self.queue.list[i] {
            for j in (1..=i).rev() {
                self.queue.list[j] = self.queue.list[j - 1];
            }
            a.order = 3;
            self.queue.list[0] = Some(a);
        }
    }

    /// Showdown's speedSort: order asc, priority desc, speed desc; ties shuffled.
    pub(crate) fn sort_queue(&mut self) {
        crate::prof!("sort_queue");
        // Drop empty entries, then an in-place selection sort with random
        // tie resolution (as Showdown does).
        let mut n = 0;
        for i in 0..self.queue.len {
            if self.queue.list[i].is_some() {
                self.queue.list[n] = self.queue.list[i];
                n += 1;
            }
        }
        for i in n..self.queue.len {
            self.queue.list[i] = None;
        }
        self.queue.len = n;
        #[inline]
        fn key(a: &Option<Action>) -> (i64, i64, i64, i64) {
            let a = a.as_ref().unwrap();
            (a.order as i64, -(a.priority as i64), -(a.frac as i64), -(a.speed as i64))
        }
        crate::rng::speed_sort(&mut self.rng, &mut self.queue.list[..n], |a, b| key(a).cmp(&key(b)));
    }

    /// Faint any Pokemon at 0 HP and check for a winner.
    pub(crate) fn process_faints(&mut self) {
        crate::prof!("process_faints");
        for c in 0..4 {
            let p = Pos::from_code(c);
            let i = self.sides[p.s()].active[p.i()];
            if i == NO_MON {
                continue;
            }
            let m = &self.sides[p.s()].mons[i as usize];
            if m.hp == 0 && !m.fainted {
                self.faint(p);
            }
        }
        self.check_win();
    }

    /// Showdown's faintMessages inside an action: faint every active Pokemon
    /// at 0 HP, checking for a winner only when asked.
    pub(crate) fn faint_pending(&mut self, check_win: bool) {
        for c in 0..4 {
            let p = Pos::from_code(c);
            let i = self.sides[p.s()].active[p.i()];
            if i == NO_MON {
                continue;
            }
            let m = &self.sides[p.s()].mons[i as usize];
            if m.hp == 0 && !m.fainted {
                self.faint(p);
            }
        }
        if check_win {
            self.check_win();
        }
    }

    /// `side.foePokemonLeft()`: the foe side still has an unfainted Pokemon.
    #[inline]
    pub(crate) fn foe_pokemon_left(&self, side: usize) -> bool {
        self.sides[1 - side].mons.iter().any(|m| m.brought && !m.fainted)
    }

    pub(crate) fn check_win(&mut self) {
        if self.ended() {
            return;
        }
        let w = match (self.sides[0].any_alive_brought(), self.sides[1].any_alive_brought()) {
            (false, false) => Some(match self.last_faint_side {
                // Gen 5+: the side whose Pokemon fainted last wins.
                Some(s) => s,
                None => 2,
            }),
            (false, true) => Some(1),
            (true, false) => Some(0),
            (true, true) => None,
        };
        if let Some(w) = w {
            blog!(self, "|win|{}", w);
            self.winner = Some(w);
            self.phase = Phase::Ended;
        }
    }

    // ---- helpers ------------------------------------------------------------

    pub fn name(&self, p: Pos) -> String {
        let i = self.sides[p.s()].active[p.i()];
        if i == NO_MON {
            return format!("p{}{}: (empty)", p.side + 1, if p.slot == 0 { 'a' } else { 'b' });
        }
        format!(
            "p{}{}: {}",
            p.side + 1,
            if p.slot == 0 { 'a' } else { 'b' },
            self.dex.sp(self.sides[p.s()].mons[i as usize].species).name
        )
    }

    pub fn mon_name(&self, side: usize, i: usize) -> &'static str {
        &self.dex.sp(self.sides[side].mons[i].species).name
    }

    /// Log lines recorded so far (cfg.log).
    pub fn log(&self) -> &[String] {
        &self.log_lines
    }

    /// Rough per-turn snapshot used by tests and the CLI.
    pub fn summary(&self) -> String {
        let mut out = format!("turn {} phase {:?}\n", self.turn, self.phase);
        for s in 0..2 {
            for i in 0..6 {
                let m = &self.sides[s].mons[i];
                if !m.brought && self.phase != Phase::TeamPreview {
                    continue;
                }
                out += &format!(
                    "  p{} {}{} {}/{} {:?} {:?}{}\n",
                    s + 1,
                    if m.active() { "*" } else { " " },
                    self.dex.sp(m.species).name,
                    m.hp,
                    m.max_hp,
                    m.status,
                    m.boosts,
                    if m.fainted { " fnt" } else { "" }
                );
            }
        }
        out
    }

    /// Default choice helper: the first legal action for every acting slot.
    pub fn first_legal_choice(&self, side: usize) -> Choice {
        let r = self.request(side);
        match r.kind {
            RequestKind::TeamPreview => Choice::preview(0),
            RequestKind::Wait => Choice::default(),
            _ => {
                let mut c = Choice::slots(PASS, PASS);
                let mut used = [false; 6];
                for slot in 0..2 {
                    let mask = self.legal_mask(side, slot);
                    for a in 0..31u8 {
                        if mask & (1 << a) == 0 {
                            continue;
                        }
                        if let SlotAction::Switch(to) = decode(a) {
                            if used[to as usize] {
                                continue;
                            }
                            used[to as usize] = true;
                        }
                        if let SlotAction::Move { mega: true, .. } = decode(a) {
                            continue;
                        }
                        c.slots[slot] = a;
                        break;
                    }
                }
                c
            }
        }
    }
}
