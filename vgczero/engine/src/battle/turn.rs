//! Showdown's turn loop, ported from the pinned sim/battle.ts (commitChoices, turnLoop,
//! runAction, endTurn, eachEvent, updateSpeed, checkFainted) and sim/battle-actions.ts
//! (switchIn, dragIn). The start, beforeTurn, runSwitch and residual steps are real queue
//! actions, so every eachEvent sort, speed update, re-sort, Emergency Exit check and switch
//! request happens where Showdown has it.

use super::ties::{Ev, Hk, Whitelist};
use super::{blog, Action, ActionKind, Battle, Phase, NO_ORIG};
use crate::actions::{decode, SlotAction};
use crate::dex::Cond;
use crate::state::{Pos, NO_MON};

/// Showdown action orders (sim/battle-queue.ts resolveAction).
pub(crate) const ORDER_START: u16 = 2;
pub(crate) const ORDER_INSTASWITCH: u16 = 3;
pub(crate) const ORDER_BEFORE_TURN: u16 = 4;
pub(crate) const ORDER_REVIVAL_BLESSING: u16 = 6;
pub(crate) const ORDER_RUN_SWITCH: u16 = 101;
pub(crate) const ORDER_RESIDUAL: u16 = 300;

impl Battle {
    /// An action without a Pokemon (`action.speed = 1`).
    pub(crate) fn marker(kind: ActionKind, order: u16) -> Action {
        Action { kind, pos: Pos::new(0, 0), mon: NO_MON, order, priority: 0, frac: 0, speed: 1, move_id: 0, loc: 0, orig: NO_ORIG }
    }

    /// The slot holds a Pokemon that has not fainted (getAllActive()).
    #[inline]
    pub(crate) fn active_unfainted(&self, p: Pos) -> bool {
        let i = self.sides[p.s()].active[p.i()];
        i != NO_MON && !self.sides[p.s()].mons[i as usize].fainted
    }

    /// battle.updateSpeed(): every unfainted active Pokemon caches its action speed.
    pub(crate) fn update_speed(&mut self) {
        crate::prof!("update_speed");
        for c in 0..4 {
            let p = Pos::from_code(c);
            if self.active_unfainted(p) {
                let s = self.action_speed(p);
                self.mm(p).speed = s;
            }
        }
    }

    /// eachEvent's order: getAllActive() speedSorted by cached speed (b.speed - a.speed).
    #[cfg_attr(feature = "trace", track_caller)]
    pub(crate) fn each_event_order(&mut self) -> ([Pos; 4], usize) {
        let mut v = [(0i32, Pos::new(0, 0)); 4];
        let mut n = 0;
        for c in 0..4 {
            let p = Pos::from_code(c);
            if self.active_unfainted(p) {
                v[n] = (self.m(p).speed, p);
                n += 1;
            }
        }
        crate::rng::speed_sort(&mut self.rng, &mut v[..n], |a, b| b.0.cmp(&a.0));
        let mut out = [Pos::new(0, 0); 4];
        for k in 0..n {
            out[k] = v[k].1;
        }
        (out, n)
    }

    /// An eachEvent whose handlers do nothing here: only the order's draws matter, and
    /// without equal cached speeds there are none.
    #[cfg_attr(feature = "trace", track_caller)]
    pub(crate) fn each_event_draws(&mut self) {
        if self.active_speed_tie() {
            let _ = self.each_event_order();
        }
    }

    /// eachEvent('Update'): each active Pokemon's Update handlers, in that order.
    #[cfg_attr(feature = "trace", track_caller)]
    pub(crate) fn each_event_update(&mut self) {
        crate::prof!("each_event_update");
        if self.update_is_quiet() {
            return;
        }
        let (ps, n) = self.each_event_order();
        for &p in &ps[..n] {
            if self.is_live(p) {
                self.update_items(p);
            }
        }
    }

    /// Two active, unfainted Pokemon share a cached speed (each_event_order would draw).
    pub(crate) fn active_speed_tie(&self) -> bool {
        let mut v = [0i32; 4];
        let mut n = 0;
        for c in 0..4 {
            let p = Pos::from_code(c);
            if self.active_unfainted(p) {
                let s = self.m(p).speed;
                if v[..n].contains(&s) {
                    return true;
                }
                v[n] = s;
                n += 1;
            }
        }
        false
    }

    /// eachEvent('Update') would be unobservable: no two active, unfainted Pokemon share a
    /// cached speed (the sort draws nothing) and no living one has a handler that can act.
    fn update_is_quiet(&self) -> bool {
        let mut v = [0i32; 4];
        let mut n = 0;
        for side in &self.sides {
            for &i in &side.active {
                if i == NO_MON {
                    continue;
                }
                let m = &side.mons[i as usize];
                if m.fainted {
                    continue;
                }
                if (m.hp > 0 && self.update_can_act(m)) || v[..n].contains(&m.speed) {
                    return false;
                }
                v[n] = m.speed;
                n += 1;
            }
        }
        true
    }

    /// EmergencyExit event (Emergency Exit / Wimp Out handlers own the half-HP test).
    pub(crate) fn emergency_exit_from(&mut self, p: Pos, original_hp: u16) {
        if !self.is_live(p) {
            return;
        }
        let m = self.m(p);
        let max = m.max_hp as u32;
        if (m.hp as u32) * 2 <= max && (original_hp as u32) * 2 > max {
            self.emergency_exit(p);
        }
    }

    /// battle-actions.ts switchIn for a chosen switch (isDrag false): BeforeSwitchOut and
    /// Update for an unfainted Pokemon leaving, its queued actions cancelled, then a runSwitch
    /// action inserted with insertChoice.
    pub(crate) fn switch_in(&mut self, p: Pos, to: usize) {
        let old = self.sides[p.s()].active[p.i()];
        let unfainted = old != NO_MON && self.sides[p.s()].mons[old as usize].hp > 0;
        if unfainted {
            if !self.m(p).vol.skip_before_switch_out {
                // runEvent('BeforeSwitchOut') has no handlers in the supported pool.
                self.each_event_update();
            }
            self.mm(p).vol.skip_before_switch_out = false;
            self.queue.remove_where(|a| a.pos == p && a.mon == old);
        }
        self.switch_out_in(p, to);
        self.insert_run_switch(p);
    }

    /// insertChoice({choice: 'runSwitch', pokemon}): the Pokemon's speed is updated first.
    pub(crate) fn insert_run_switch(&mut self, p: Pos) {
        let s = self.action_speed(p);
        self.mm(p).speed = s;
        let mut a = Self::marker(ActionKind::RunSwitch, ORDER_RUN_SWITCH);
        a.pos = p;
        a.mon = self.sides[p.s()].active[p.i()];
        a.speed = s;
        self.insert_choice(a);
    }

    /// commitChoices for move requests: updateSpeed, each side's actions resolved in slot
    /// order, queue.sort(), then turnLoop.
    pub(crate) fn commit_moves(&mut self, choices: &[crate::actions::Choice; 2]) {
        self.update_speed();
        self.build_queue(choices);
        self.turn_loop();
    }

    /// commitChoices for switch requests: instaswitch actions are resolved and sorted, and the
    /// saved rest of the turn is appended unsorted.
    pub(crate) fn commit_switches(&mut self, choices: &[crate::actions::Choice; 2], revive_pass: [[bool; 2]; 2]) {
        crate::prof!("commit_switches");
        self.update_speed();
        let old = self.queue;
        self.queue = super::Queue::default();
        for s in 0..2 {
            for slot in 0..2 {
                if !self.switch_slots[s][slot] {
                    continue;
                }
                let p = Pos::new(s, slot);
                if let SlotAction::Switch(to) = decode(choices[s].slots[slot]) {
                    let mut a = if self.sides[s].reviving[slot] {
                        Self::marker(ActionKind::RevivalBlessing { to }, ORDER_REVIVAL_BLESSING)
                    } else {
                        Self::marker(ActionKind::InstaSwitch { to }, ORDER_INSTASWITCH)
                    };
                    a.pos = p;
                    a.mon = self.sides[s].active[slot];
                    a.speed = self.action_speed(p);
                    self.queue.push(a);
                }
            }
        }
        self.sort_queue();
        for a in old.iter() {
            self.queue.push(*a);
        }
        self.switch_slots = [[false; 2]; 2];
        self.phase = Phase::Move;
        for s in 0..2 {
            for slot in 0..2 {
                let p = Pos::new(s, slot);
                if revive_pass[s][slot] && self.is_live(p) {
                    self.mm(p).vol.switch_flag = true;
                }
            }
        }
        self.turn_loop();
    }

    /// turnLoop: a new turn gets beforeTurn and residual actions; actions run until a
    /// request or the end; an empty queue ends the turn.
    pub(crate) fn turn_loop(&mut self) {
        if !self.mid_turn {
            self.insert_choice(Self::marker(ActionKind::BeforeTurn, ORDER_BEFORE_TURN));
            self.queue.push(Self::marker(ActionKind::Residual, ORDER_RESIDUAL));
            self.mid_turn = true;
        }
        while let Some(a) = self.queue.pop_front() {
            if self.run_action(a) {
                return;
            }
        }
        self.mid_turn = false;
        self.turn += 1;
        self.begin_turn();
    }

    /// runAction. Returns true when a request was made or the battle ended.
    pub(crate) fn run_action(&mut self, a: Action) -> bool {
        crate::prof!("run_action");
        let original_hp = if a.mon != NO_MON { self.sides[a.pos.s()].mons[a.mon as usize].hp } else { 0 };
        let mut residual_hp: Option<([(Pos, u16); 4], usize)> = None;
        match a.kind {
            ActionKind::Start => {
                for s in 0..2 {
                    for slot in 0..2 {
                        let p = Pos::new(s, slot);
                        blog!(self, "|switch|{}|{}/{}", self.name(p), self.m(p).hp, self.m(p).max_hp);
                        self.insert_run_switch(p);
                    }
                }
                self.mid_turn = true;
            }
            ActionKind::BeforeTurn => {
                // eachEvent('BeforeTurn'): no handlers in the supported pool, but the sort draws.
                self.each_event_draws();
            }
            ActionKind::Switch { to } | ActionKind::InstaSwitch { to } => {
                let live_actor = self.sides[a.pos.s()].active[a.pos.i()] == a.mon;
                if live_actor && self.sides[a.pos.s()].can_switch_to(to as usize) {
                    self.switch_in(a.pos, to as usize);
                }
            }
            ActionKind::RevivalBlessing { to } => {
                // The revived Pokemon heals to half; one still in its slot comes straight back
                // in (Showdown adds an instaswitch for it).
                if let Some(q) = self.revive(a.pos, to as usize) {
                    self.insert_run_switch(q);
                }
            }
            ActionKind::RunSwitch => {
                // runSwitch takes every consecutive runSwitch action.
                let mut switchers = [a.pos; 4];
                let mut n = 1;
                loop {
                    let next = self.queue.iter().next().copied();
                    let Some(next) = next.filter(|x| x.kind == ActionKind::RunSwitch) else { break };
                    self.queue.pop_front();
                    switchers[n] = next.pos;
                    n += 1;
                }
                self.run_switch_in(&switchers[..n]);
            }
            ActionKind::Mega => {
                if self.sides[a.pos.s()].active[a.pos.i()] == a.mon && self.is_live(a.pos) {
                    self.mega_evolve(a.pos);
                }
            }
            ActionKind::Move { mslot, recharge, .. } => {
                // runAction returns at once (no Update, no re-sort) for a Pokemon that is no
                // longer active or has fainted.
                if self.sides[a.pos.s()].active[a.pos.i()] != a.mon || !self.active_unfainted(a.pos) {
                    return false;
                }
                self.run_move_action(a.pos, mslot as usize, a.loc, a.orig, a.move_id, recharge);
            }
            ActionKind::Residual => {
                self.update_speed();
                let mut hp = [(Pos::new(0, 0), 0u16); 4];
                let mut n = 0;
                for c in 0..4 {
                    let p = Pos::from_code(c);
                    if self.active_unfainted(p) {
                        hp[n] = (p, self.m(p).hp);
                        n += 1;
                    }
                }
                residual_hp = Some((hp, n));
                self.run_residual();
            }
        }
        if self.ended() {
            return true;
        }
        // Phazing (Roar, Dragon Tail, Red Card).
        for c in 0..4 {
            let p = Pos::from_code(c);
            if self.sides[p.s()].active[p.i()] != NO_MON && self.m(p).vol.force_switch {
                self.mm(p).vol.force_switch = false;
                if self.is_live(p) {
                    self.drag_out(p);
                }
            }
        }
        // faintMessages.
        self.process_faints();
        if self.ended() {
            return true;
        }
        if self.queue.len == 0 {
            self.check_fainted();
        } else if self.queue.iter().next().is_some_and(|n| matches!(n.kind, ActionKind::InstaSwitch { .. })) {
            return false;
        }
        if a.kind != ActionKind::Start {
            self.each_event_update();
            if let Some((hp, n)) = residual_hp {
                for &(p, h) in &hp[..n] {
                    self.emergency_exit_from(p, h);
                }
            }
        }
        if a.kind == ActionKind::RunSwitch {
            self.emergency_exit_from(a.pos, original_hp);
        }
        if self.request_switches() {
            return true;
        }
        if self.queue.iter().next().is_some_and(|n| matches!(n.kind, ActionKind::Move { .. })) {
            self.update_speed();
            self.update_queue_speeds();
            self.sort_queue();
        }
        false
    }

    /// checkFainted: every fainted active Pokemon must be replaced.
    fn check_fainted(&mut self) {
        for c in 0..4 {
            let p = Pos::from_code(c);
            let i = self.sides[p.s()].active[p.i()];
            if i != NO_MON && self.sides[p.s()].mons[i as usize].fainted {
                self.sides[p.s()].mons[i as usize].vol.switch_flag = true;
            }
        }
    }

    /// runAction's switch requests: flags without a replacement are dropped (Revival
    /// Blessing keeps its slot); otherwise a switch request is made.
    fn request_switches(&mut self) -> bool {
        let mut switches = [false; 2];
        for s in 0..2 {
            for slot in 0..2 {
                let i = self.sides[s].active[slot];
                if i != NO_MON && self.sides[s].mons[i as usize].vol.switch_flag {
                    switches[s] = true;
                }
            }
        }
        for s in 0..2 {
            if switches[s] && self.sides[s].bench_available() == 0 {
                let mut revive = false;
                for slot in 0..2 {
                    if self.sides[s].reviving[slot] {
                        revive = true;
                        continue;
                    }
                    let i = self.sides[s].active[slot];
                    if i != NO_MON {
                        self.sides[s].mons[i as usize].vol.switch_flag = false;
                    }
                }
                if !revive {
                    switches[s] = false;
                }
            } else if switches[s] {
                for slot in 0..2 {
                    let p = Pos::new(s, slot);
                    if self.is_live(p) && self.m(p).vol.switch_flag && !self.sides[s].reviving[slot] && !self.m(p).vol.skip_before_switch_out {
                        // runEvent('BeforeSwitchOut'): no handlers in the supported pool.
                        self.mm(p).vol.skip_before_switch_out = true;
                        self.process_faints();
                        if self.ended() {
                            return true;
                        }
                    }
                }
            }
        }
        if !switches[0] && !switches[1] {
            return false;
        }
        let mut slots = [[false; 2]; 2];
        for s in 0..2 {
            for slot in 0..2 {
                let i = self.sides[s].active[slot];
                if i != NO_MON && self.sides[s].mons[i as usize].vol.switch_flag {
                    slots[s][slot] = true;
                    // (A Revival Blessing user keeps its flag until it picks.)
                    if !self.sides[s].reviving[slot] {
                        self.sides[s].mons[i as usize].vol.switch_flag = false;
                    }
                }
            }
        }
        self.switch_slots = slots;
        let midturn = self.queue.iter().any(|a| a.kind == ActionKind::Residual);
        self.phase = Phase::Switch { midturn };
        true
    }
}

/// One runEvent handler for the end-of-turn sorts: (priority, cached speed, subOrder, holder
/// position code or 4 for the field, effect rank).
type Eh = (i32, i32, u8, u8, u16);

impl Battle {
    /// runEvent's speedSort of the handlers it found: whitelisted artificial ties keep a fixed
    /// order, other ties draw.
    fn sort_event_handlers(&mut self, event: Ev, list: &mut [Eh]) {
        let wl = &self.dex.whitelist;
        crate::rng::speed_sort_with(
            &mut self.rng,
            list,
            |a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)),
            |g| {
                if !g.iter().all(|h| h.3 == g[0].3) {
                    return false;
                }
                if g.len() > Whitelist::MAX_EFFECTS {
                    return false;
                }
                let kind = if g[0].3 == 4 { Hk::Field } else { Hk::Pokemon };
                let mut ids = [0u16; Whitelist::MAX_EFFECTS];
                for (k, h) in g.iter().enumerate() {
                    ids[k] = h.4;
                }
                wl.contains(event, &ids[..g.len()], kind)
            },
            |a, b| a.4.cmp(&b.4),
        );
    }

    /// endTurn: runEvent('DisableMove') and runEvent('TrapPokemon') for each active Pokemon.
    /// The engine applies their effects where it checks moves and switching; only the handler
    /// sorts consume randomness here.
    pub(crate) fn end_turn_event_sorts(&mut self) {
        crate::prof!("end_turn_event_sorts");
        use crate::kinds::Ab;
        let d = self.dex;
        for c in 0..4u8 {
            let p = Pos::from_code(c);
            if !self.active_unfainted(p) {
                continue;
            }
            let (v, spd) = (self.m(p).vol, self.m(p).speed);
            let mut h: crate::fixed::FixedVec<Eh, 12> = crate::fixed::FixedVec::new();
            let mut push = |e: Eh| h.push(e);
            // DisableMove: own volatiles (subOrder 2), Imprison of the foes, Gravity (field).
            if v.choice_lock > 0 {
                push((0, spd, 2, c, d.cr(Cond::ChoiceLock)));
            }
            if v.disable > 0 {
                push((0, spd, 2, c, d.cr(Cond::Disable)));
            }
            if v.encore > 0 {
                push((0, spd, 2, c, d.cr(Cond::Encore)));
            }
            if v.taunt > 0 {
                push((0, spd, 2, c, d.cr(Cond::Taunt)));
            }
            if v.throat_chop > 0 {
                push((0, spd, 2, c, d.cr(Cond::ThroatChop)));
            }
            if v.torment {
                push((0, spd, 2, c, d.cr(Cond::Torment)));
            }
            if v.heal_block > 0 {
                push((0, spd, 2, c, d.cr(Cond::HealBlock)));
            }
            for slot in 0..2 {
                let q = p.foe(slot);
                if self.active_unfainted(q) && self.m(q).vol.imprison {
                    push((0, self.m(q).speed, 2, q.code(), d.cr(Cond::Imprison)));
                }
            }
            if self.field.gravity > 0 {
                push((0, 0, 5, 4, d.cr(Cond::Gravity)));
            }
            self.sort_event_handlers(Ev::DisableMove, &mut h);
            if v.choice_lock > 0 && !super::legal::is_choice(self.it(p)) {
                self.mm(p).vol.choice_lock = 0;
            }
            // TrapPokemon: partial trapping, No Retreat's trapped, Ingrain, Run Away (-10),
            // the foes' Shadow Tag.
            let mut h: crate::fixed::FixedVec<Eh, 12> = crate::fixed::FixedVec::new();
            let mut push = |e: Eh| h.push(e);
            if v.partial_trap > 0 {
                push((0, spd, 2, c, d.cr(Cond::PartiallyTrapped)));
            }
            if v.no_retreat {
                push((0, spd, 2, c, d.cr(Cond::Trapped)));
            }
            if v.ingrain {
                push((0, spd, 2, c, d.cr(Cond::Ingrain)));
            }
            if self.ab(p) == Ab::RunAway {
                push((-10, spd, 7, c, d.cr(Cond::RunAway)));
            }
            for slot in 0..2 {
                let q = p.foe(slot);
                if self.active_unfainted(q) && self.ab(q) == Ab::ShadowTag {
                    push((0, self.m(q).speed, 7, q.code(), d.cr(Cond::ShadowTag)));
                }
            }
            self.sort_event_handlers(Ev::TrapPokemon, &mut h);
        }
    }
}
