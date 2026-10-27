//! Compact primitives for the first kernel slice. This is not yet the full exchange-world
//! importer: effects, requests, action resolution and the complete battle loop are pending.
use crate::position::{MemberId, PositionError, Roster, Side};
use crate::rng::SodiumRng;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Member {
    pub species: u16,
    pub ability: u16,
    pub item: u16,
    pub hp: u16,
    pub max_hp: u16,
    pub stats: [u16; 5],
    pub stages: [i8; 7],
    pub moves: [MoveSlot; 4],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MoveSlot { pub id: u16, pub pp: u8, pub max_pp: u8 }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InformationPolicy { OpenSheets, ClosedSheets }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileId {
    pub rules: [u8; 32],
    pub information: InformationPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct State<const ACTIVE: usize> {
    profile: ProfileId,
    rng: SodiumRng,
    members: [[Member; 6]; 2],
    rosters: [Roster<ACTIVE>; 2],
    resources_used: [u32; 2],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Snapshot<const ACTIVE: usize> {
    layout: u16,
    state: State<ACTIVE>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateError { Position(PositionError), InvalidMember, ProfileMismatch, ResourceUsed, InvalidResource, Fainted }

impl From<PositionError> for StateError {
    fn from(error: PositionError) -> Self { Self::Position(error) }
}

impl<const ACTIVE: usize> State<ACTIVE> {
    pub fn new(profile: ProfileId, seed: [u8; 32], members: [[Member; 6]; 2]) -> Result<Self, StateError> {
        for member in members.iter().flatten() {
            if member.species == 0 || member.max_hp == 0 || member.hp > member.max_hp
                || member.stats.contains(&0) || member.stages.iter().any(|value| !(-6..=6).contains(value))
                || member.moves.iter().any(|slot| slot.pp > slot.max_pp || (slot.id == 0 && (slot.pp != 0 || slot.max_pp != 0))) {
                return Err(StateError::InvalidMember);
            }
        }
        Ok(Self {profile, rng: SodiumRng::new(seed), members, rosters: [Roster::new()?, Roster::new()?], resources_used: [0; 2]})
    }
    pub fn select(&mut self, side: Side, members: &[MemberId]) -> Result<(), StateError> {
        self.rosters[side.index()].select(members)?;
        Ok(())
    }
    pub fn roster(&self, side: Side) -> &Roster<ACTIVE> { &self.rosters[side.index()] }
    pub fn member(&self, side: Side, member: MemberId) -> &Member { &self.members[side.index()][member.index()] }
    pub fn rng(&mut self) -> &mut SodiumRng { &mut self.rng }
    pub fn switch(&mut self, side: Side, slot: u8, incoming: MemberId) -> Result<MemberId, StateError> {
        if self.member(side, incoming).hp == 0 { return Err(StateError::Fainted); }
        self.rosters[side.index()].switch(slot, incoming).map_err(StateError::from)
    }
    /// HP accounting only. Faint queues, callbacks and victory are handled by the later phase machine.
    pub fn lose_hp(&mut self, side: Side, member: MemberId, damage: u16) -> u16 {
        let hp = &mut self.members[side.index()][member.index()].hp;
        let applied = damage.min(*hp);
        *hp -= applied;
        applied
    }
    pub fn spend_resource(&mut self, side: Side, resource: u8) -> Result<(), StateError> {
        if resource >= 32 { return Err(StateError::InvalidResource); }
        let bit = 1u32 << resource;
        if self.resources_used[side.index()] & bit != 0 { return Err(StateError::ResourceUsed); }
        self.resources_used[side.index()] |= bit;
        Ok(())
    }
    pub fn snapshot(&self) -> Snapshot<ACTIVE> { Snapshot {layout: 1, state: *self} }
    pub fn restore(profile: ProfileId, snapshot: Snapshot<ACTIVE>) -> Result<Self, StateError> {
        if snapshot.layout != 1 || snapshot.state.profile != profile { return Err(StateError::ProfileMismatch); }
        Ok(snapshot.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    fn member() -> Member {
        Member {species: 1, ability: 0, item: 0, hp: 100, max_hp: 100,
            stats: [100; 5], stages: [0; 7], moves: [MoveSlot {id: 1, pp: 10, max_pp: 10}; 4]}
    }

    fn exercise<const ACTIVE: usize>(information: InformationPolicy) {
        let profile = ProfileId {rules: [3; 32], information};
        let mut state = State::<ACTIVE>::new(profile, [7; 32], [[member(); 6]; 2]).unwrap();
        let ids = [0, 1, 2, 3].map(|index| MemberId::new(index).unwrap());
        for side in [Side::One, Side::Two] { state.select(side, &ids[..if ACTIVE == 1 {3} else {4}]).unwrap(); }
        let before = state.snapshot();
        let mut left = State::restore(profile, before).unwrap();
        let mut right = State::restore(profile, before).unwrap();
        assert_eq!(left.switch(Side::One, 0, ids[2]).unwrap(), ids[0]);
        assert_eq!(left.lose_hp(Side::One, ids[2], 1000), 100);
        left.spend_resource(Side::One, 0).unwrap();
        left.spend_resource(Side::One, 1).unwrap();
        assert_eq!(left.spend_resource(Side::One, 0), Err(StateError::ResourceUsed));
        assert_eq!(left.spend_resource(Side::One, 32), Err(StateError::InvalidResource));
        left.rng().next_u32();
        assert_eq!(right.snapshot(), before);
        assert_eq!(state.snapshot(), before);
        right.rng().next_u32();
        assert_eq!(right.rng.state(), left.rng.state());
        assert_eq!(State::restore(ProfileId {rules: [0; 32], information}, before), Err(StateError::ProfileMismatch));
        assert!(size_of::<State<ACTIVE>>() < 8192);
    }

    #[test]
    fn primitive_state_forks_restore_in_all_format_information_combinations() {
        for information in [InformationPolicy::OpenSheets, InformationPolicy::ClosedSheets] {
            exercise::<1>(information); exercise::<2>(information);
        }
    }

    #[test]
    fn duplicate_selection_and_rejected_switches_leave_state_unchanged() {
        let mut roster = Roster::<2>::new().unwrap();
        let ids = [0, 1, 2, 3, 4].map(|index| MemberId::new(index).unwrap());
        let before = roster;
        assert_eq!(roster.select(&[ids[0], ids[0], ids[2], ids[3]]), Err(PositionError::InvalidSelection));
        assert_eq!(roster, before);
        roster.select(&ids[..4]).unwrap();
        let before = roster;
        assert_eq!(roster.switch(0, ids[1]), Err(PositionError::AlreadyActive));
        assert_eq!(roster.switch(0, ids[4]), Err(PositionError::NotSelected));
        assert_eq!(roster.switch(2, ids[2]), Err(PositionError::InvalidSlot));
        assert_eq!(roster, before);
        assert_eq!(Roster::<3>::new(), Err(PositionError::InvalidFormat));
    }
}
