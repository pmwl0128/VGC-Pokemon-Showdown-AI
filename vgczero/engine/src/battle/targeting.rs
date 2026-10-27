//! Showdown target resolution for doubles, ported from the pinned sim/battle.ts
//! (getTarget, getRandomTarget, validTargetLoc), sim/pokemon.ts (getMoveTargets) and
//! sim/battle-queue.ts (resolveAction). Locations are Showdown's: foe slot k is k + 1, own
//! slot k is -(k + 1), 0 means no chosen target. Every Showdown `sample` is one `below(n)`
//! draw here, including n = 1.

use super::Battle;
use crate::dex::{flag, MoveId, Target};
use crate::kinds::Ab;
use crate::state::{Pos, NO_MON};

/// No original target recorded (`action.originalTarget` undefined).
pub const NO_ORIG: u8 = 255;

impl Battle {
    /// `pokemon.getLocOf(target)`.
    #[inline]
    pub(crate) fn loc_of(user: Pos, t: Pos) -> i8 {
        let pos = t.slot as i8 + 1;
        if t.side == user.side {
            -pos
        } else {
            pos
        }
    }

    /// `pokemon.getAtLoc(loc)`: the slot, whether or not it holds a living Pokemon.
    #[inline]
    pub(crate) fn at_loc(user: Pos, loc: i8) -> Option<Pos> {
        match loc {
            1 | 2 => Some(user.foe((loc - 1) as usize)),
            -1 | -2 => Some(Pos { side: user.side, slot: (-loc - 1) as u8 }),
            _ => None,
        }
    }

    #[inline]
    fn slot_mon(&self, p: Pos) -> Option<&crate::state::Mon> {
        let i = self.sides[p.s()].active[p.i()];
        (i != NO_MON).then(|| &self.sides[p.s()].mons[i as usize])
    }

    /// Identity of the Pokemon in a slot, for `action.originalTarget`.
    #[inline]
    pub(crate) fn orig_code(&self, p: Option<Pos>) -> u8 {
        match p {
            Some(p) if self.sides[p.s()].active[p.i()] != NO_MON => p.side * 6 + self.sides[p.s()].active[p.i()],
            _ => NO_ORIG,
        }
    }

    /// Where the Pokemon behind an `orig_code` is active now (`isActive`: not fainted).
    fn active_slot_of(&self, orig: u8) -> Option<Pos> {
        if orig == NO_ORIG {
            return None;
        }
        let (side, mon) = ((orig / 6) as usize, orig % 6);
        (0..2).map(|slot| Pos::new(side, slot)).find(|&q| self.sides[side].active[q.i()] == mon && !self.sides[side].mons[mon as usize].fainted)
    }

    /// resolveAction / side.chooseMove: in doubles these target types carry the player's
    /// chosen location; every other type is resolved with getRandomTarget.
    #[inline]
    pub(crate) fn target_chosen(t: Target) -> bool {
        matches!(t, Target::Normal | Target::Any | Target::AdjacentAlly | Target::AdjacentAllyOrSelf | Target::AdjacentFoe)
    }

    /// `battle.getRandomTarget(pokemon, move)` (doubles: activePerHalf = 2).
    #[cfg_attr(feature = "trace", track_caller)]
    pub(crate) fn get_random_target(&mut self, p: Pos, t: Target) -> Option<Pos> {
        match t {
            Target::User | Target::All | Target::AllySide | Target::AllyTeam | Target::AdjacentAllyOrSelf => Some(p),
            Target::AdjacentAlly => {
                // adjacentAllies(): the ally with HP (isAdjacent excludes the user and the fainted).
                let a = p.ally();
                if self.slot_mon(a).is_some_and(|m| m.hp > 0 && !m.fainted) && self.slot_mon(p).is_some_and(|m| !m.fainted) {
                    self.rng.below(1);
                    Some(a)
                } else {
                    None
                }
            }
            _ => {
                // side.randomFoe() || side.foe.active[0]
                let mut foes = [p; 2];
                let mut n = 0;
                for slot in 0..2 {
                    let q = p.foe(slot);
                    if self.slot_mon(q).is_some_and(|m| m.hp > 0) {
                        foes[n] = q;
                        n += 1;
                    }
                }
                if n > 0 {
                    Some(foes[self.rng.below(n as u32) as usize])
                } else {
                    self.slot_mon(p.foe(0)).map(|_| p.foe(0))
                }
            }
        }
    }

    /// `battle.validTargetLoc(targetLoc, source, targetType)` for doubles.
    pub(crate) fn valid_target_loc(loc: i8, src: Pos, t: Target) -> bool {
        if loc == 0 {
            return true;
        }
        if loc.unsigned_abs() > 2 {
            return false;
        }
        let src_loc = Self::loc_of(src, src);
        let is_self = src_loc == loc;
        let is_foe = loc > 0;
        let across = -(2 + 1 - loc);
        let adjacent = if loc > 0 { (across - src_loc).abs() <= 1 } else { (loc - src_loc).abs() == 1 };
        match t {
            Target::RandomNormal | Target::Scripted | Target::Normal => adjacent,
            Target::AdjacentAlly => adjacent && !is_foe,
            Target::AdjacentAllyOrSelf => (adjacent && !is_foe) || is_self,
            Target::AdjacentFoe => adjacent && is_foe,
            Target::Any => !is_self,
            _ => false,
        }
    }

    /// `battle.getTarget(pokemon, move, targetLoc, originalTarget)`.
    #[cfg_attr(feature = "trace", track_caller)]
    pub(crate) fn get_target(&mut self, p: Pos, move_id: MoveId, loc: i8, orig: u8) -> Option<Pos> {
        crate::prof!("get_target");
        let mv = self.dex.mv(move_id);
        let target = self.action_target_type(p, move_id);
        // tracksTarget moves are unsupported; Stalwart tracks here (Propeller Tail is not in
        // the ability table, so its teams are unsupported).
        if self.ab(p) == Ab::Stalwart {
            if let Some(q) = self.active_slot_of(orig) {
                return Some(q);
            }
        }
        let self_loc = Self::loc_of(p, p);
        if matches!(target, Target::AdjacentAlly | Target::Any | Target::Normal) && loc == self_loc && self.m(p).vol.charging == 0 {
            return if mv.has(flag::FUTUREMOVE) { Some(p) } else { None };
        }
        if target != Target::RandomNormal && Self::valid_target_loc(loc, p, target) {
            if let Some(t) = Self::at_loc(p, loc) {
                if let Some(m) = self.slot_mon(t) {
                    if m.fainted {
                        if t.side == p.side {
                            return if target == Target::AdjacentAllyOrSelf { Some(p) } else { Some(t) };
                        }
                    } else {
                        return Some(t);
                    }
                }
            }
        }
        self.get_random_target(p, target)
    }

    /// The move's target type as the queued action sees it (Champions: a non-Ghost Curse
    /// targets the user; battle-queue.ts resolveAction).
    pub(crate) fn action_target_type(&self, p: Pos, move_id: MoveId) -> Target {
        let mv = self.dex.mv(move_id);
        if mv.fx == crate::kinds::MoveFx::Curse && !self.m(p).has_type(crate::types::Type::Ghost) {
            Target::User
        } else {
            mv.target
        }
    }

    /// resolveAction's targetLoc / originalTarget. `code` is the player's chosen target, or
    /// None for a locked (charging) move, which keeps its stored location.
    #[cfg_attr(feature = "trace", track_caller)]
    pub(crate) fn resolve_target(&mut self, p: Pos, move_id: MoveId, code: Option<u8>) -> (i8, u8) {
        let t = self.action_target_type(p, move_id);
        let mut loc = match code {
            None => self.m(p).vol.charge_target,
            // changeAction (Encore): no targetLoc, so resolveAction picks one at random.
            Some(super::TARGET_RANDOM) => 0,
            Some(c) if Self::target_chosen(t) => Self::loc_from_code(p, t, c),
            Some(_) => 0,
        };
        if loc == 0 {
            if let Some(q) = self.get_random_target(p, t) {
                loc = Self::loc_of(p, q);
            }
        }
        (loc, self.orig_code(Self::at_loc(p, loc)))
    }

    /// Showdown's `targetLoc` for an engine target code (crate::actions): foe-or-ally moves
    /// use 0/1 for the foe slots and 2 for the ally; an adjacentAlly move always aims at the
    /// ally; adjacentAllyOrSelf uses 2 for the ally and 0 for the user.
    pub(crate) fn loc_from_code(p: Pos, t: Target, code: u8) -> i8 {
        match (t, code) {
            (Target::AdjacentAlly, _) | (Target::AdjacentAllyOrSelf, 2) => Self::loc_of(p, p.ally()),
            (Target::AdjacentAllyOrSelf, _) => Self::loc_of(p, p),
            (_, 0) => 1,
            (_, 1) => 2,
            (_, 2) => Self::loc_of(p, p.ally()),
            _ => 0,
        }
    }
}
