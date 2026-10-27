//! Joint-choice checks consume an already split player request. This type deliberately
//! has no opponent stats, true trap state or opponent pending choices.
use crate::position::{valid_target, ActiveSlot, MemberId, Side, TargetKind, TargetLoc};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestedMove { pub id: u16, pub target: TargetKind, pub disabled: bool }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestedSlot {
    pub moves: [RequestedMove; 4],
    pub move_count: u8,
    pub pass_only: bool,
    /// Only the trap flag disclosed by the request; unknown traps must remain unknown.
    pub trapped: bool,
    pub can_mega: bool,
    pub force_switch: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnMember { pub id: MemberId, pub fainted: bool, pub reviving: bool }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecisionPhase { Move, Switch }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlayerMenu<const ACTIVE: usize> {
    pub side: Side,
    pub phase: DecisionPhase,
    /// Request order, with the active members first. IDs refer to the initial own roster.
    pub members: [OwnMember; 6],
    pub member_count: u8,
    pub slots: [RequestedSlot; ACTIVE],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Choice {
    Move { index: u8, target: TargetLoc, mega: bool },
    Switch { member: MemberId },
    Pass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Action { pub actor: ActiveSlot, pub kind: ActionKind }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionKind {
    Move { id: u16, index: u8, target: TargetLoc, resource: Option<u8> },
    Switch { member: MemberId },
    Pass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChoiceError { InvalidMenu, UnsupportedMechanism, IllegalMove, IllegalTarget, IllegalSwitch, IllegalPass, MegaUnavailable, SharedResource, SwitchBudget }

impl<const ACTIVE: usize> PlayerMenu<ACTIVE> {
    pub fn validate(&self, choices: [Choice; ACTIVE]) -> Result<[Action; ACTIVE], ChoiceError> {
        if ![1, 2].contains(&ACTIVE) || usize::from(self.member_count) < ACTIVE || self.member_count > 6 {
            return Err(ChoiceError::InvalidMenu);
        }
        let members = &self.members[..usize::from(self.member_count)];
        if members.iter().any(|member| member.reviving) { return Err(ChoiceError::UnsupportedMechanism); }
        for (index, member) in members.iter().enumerate() {
            if members[..index].iter().any(|other| other.id == member.id) { return Err(ChoiceError::InvalidMenu); }
        }
        let mut switch_ids = 0u8;
        let mut mega_used = false;
        let mut switches = 0;
        let mut forced_passes = 0;
        let required = self.slots.iter().filter(|slot| slot.force_switch).count();
        let available = members[ACTIVE..].iter().filter(|member| !member.fainted).count();
        let mut actions = [Action {actor: ActiveSlot {side: self.side, index: 0}, kind: ActionKind::Pass}; ACTIVE];
        for (index, (choice, slot)) in choices.iter().zip(&self.slots).enumerate() {
            let actor = ActiveSlot {side: self.side, index: index as u8};
            if slot.move_count > 4 { return Err(ChoiceError::InvalidMenu); }
            let kind = match *choice {
                Choice::Move {index, target, mega} => {
                    if self.phase != DecisionPhase::Move || slot.pass_only || members[actor.index as usize].fainted || index >= slot.move_count {
                        return Err(ChoiceError::IllegalMove);
                    }
                    let movement = slot.moves[index as usize];
                    if movement.id == 0 || movement.disabled { return Err(ChoiceError::IllegalMove); }
                    let selectable = matches!(movement.target, TargetKind::Normal | TargetKind::Any | TargetKind::AdjacentAlly
                        | TargetKind::AdjacentAllyOrSelf | TargetKind::AdjacentFoe);
                    if (!selectable && target.value() != 0) || (selectable && ACTIVE > 1 && target.value() == 0) {
                        return Err(ChoiceError::IllegalTarget);
                    }
                    if !valid_target::<ACTIVE>(actor, target, movement.target) { return Err(ChoiceError::IllegalTarget); }
                    if mega {
                        if !slot.can_mega { return Err(ChoiceError::MegaUnavailable); }
                        if mega_used { return Err(ChoiceError::SharedResource); }
                        mega_used = true;
                    }
                    ActionKind::Move {id: movement.id, index, target, resource: if mega {Some(0)} else {None}}
                }
                Choice::Switch {member} => {
                    if (self.phase == DecisionPhase::Move && (slot.pass_only || members[index].fainted || slot.trapped))
                        || (self.phase == DecisionPhase::Switch && !slot.force_switch)
                        || !members[ACTIVE..].iter().any(|candidate| candidate.id == member && !candidate.fainted) {
                        return Err(ChoiceError::IllegalSwitch);
                    }
                    let bit = 1u8 << member.index();
                    if switch_ids & bit != 0 { return Err(ChoiceError::SharedResource); }
                    switch_ids |= bit;
                    switches += 1;
                    ActionKind::Switch {member}
                }
                Choice::Pass => {
                    if self.phase == DecisionPhase::Move {
                        if !slot.pass_only && !members[index].fainted { return Err(ChoiceError::IllegalPass); }
                    } else if slot.force_switch { forced_passes += 1; }
                    ActionKind::Pass
                }
            };
            actions[index] = Action {actor, kind};
        }
        if self.phase == DecisionPhase::Switch && (switches != required.min(available) || forced_passes != required.saturating_sub(available)) {
            return Err(ChoiceError::SwitchBudget);
        }
        Ok(actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu() -> PlayerMenu<2> {
        let movement = RequestedMove {id: 1, target: TargetKind::Normal, disabled: false};
        let slot = RequestedSlot {moves: [movement; 4], move_count: 4, pass_only: false, trapped: false, can_mega: true, force_switch: false};
        PlayerMenu {side: Side::One, phase: DecisionPhase::Move, member_count: 4,
            members: std::array::from_fn(|index| OwnMember {id: MemberId::new(index as u8).unwrap(), fainted: false, reviving: false}), slots: [slot; 2]}
    }

    #[test]
    fn joint_menu_preserves_shared_resources_targets_and_disclosed_traps() {
        let mut menu = menu();
        let target = TargetLoc::new::<2>(1).unwrap();
        let movement = Choice::Move {index: 0, target, mega: false};
        let mega = Choice::Move {index: 0, target, mega: true};
        assert!(menu.validate([mega, movement]).is_ok());
        assert_eq!(menu.validate([mega, mega]), Err(ChoiceError::SharedResource));
        let incoming = menu.members[2].id;
        let switch = Choice::Switch {member: incoming};
        assert_eq!(menu.validate([switch, switch]), Err(ChoiceError::SharedResource));
        assert!(menu.validate([switch, movement]).is_ok());
        menu.slots[0].trapped = true;
        assert_eq!(menu.validate([switch, movement]), Err(ChoiceError::IllegalSwitch));
        assert_eq!(menu.validate([Choice::Pass, movement]), Err(ChoiceError::IllegalPass));
        let self_target = Choice::Move {index: 0, target: TargetLoc::new::<2>(-1).unwrap(), mega: false};
        assert_eq!(menu.validate([self_target, movement]), Err(ChoiceError::IllegalTarget));
        assert_eq!(menu.validate([Choice::Move {index: 4, target, mega: false}, movement]), Err(ChoiceError::IllegalMove));
        assert_eq!(menu.validate([Choice::Move {index: 0, target: TargetLoc::AUTO, mega: false}, movement]), Err(ChoiceError::IllegalTarget));
        menu.members[0].reviving = true;
        assert_eq!(menu.validate([movement, movement]), Err(ChoiceError::UnsupportedMechanism));
    }

    #[test]
    fn forced_replacement_uses_all_available_bench_members_and_passes_only_the_deficit() {
        let mut menu = menu(); menu.phase = DecisionPhase::Switch;
        menu.slots[0].force_switch = true; menu.slots[1].force_switch = true;
        menu.members[3].fainted = true;
        let switch = Choice::Switch {member: menu.members[2].id};
        assert!(menu.validate([switch, Choice::Pass]).is_ok());
        assert!(menu.validate([Choice::Pass, switch]).is_ok());
        assert_eq!(menu.validate([Choice::Pass, Choice::Pass]), Err(ChoiceError::SwitchBudget));
        menu.slots[0].force_switch = false;
        assert_eq!(menu.validate([switch, Choice::Pass]), Err(ChoiceError::IllegalSwitch));
        assert!(menu.validate([Choice::Pass, switch]).is_ok());
    }
}
