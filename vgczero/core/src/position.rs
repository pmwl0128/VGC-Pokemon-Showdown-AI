//! Two-sided single/double topology. Member IDs are stable initial roster indices;
//! active slot indices and Showdown target locations are separate types.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Side {
    One = 0,
    Two = 1,
}

impl Side {
    pub const fn foe(self) -> Self {
        match self { Self::One => Self::Two, Self::Two => Self::One }
    }
    pub const fn index(self) -> usize { self as usize }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemberId(u8);
impl MemberId {
    pub fn new(index: u8) -> Result<Self, PositionError> {
        if index < 6 { Ok(Self(index)) } else { Err(PositionError::InvalidMember) }
    }
    pub const fn index(self) -> usize { self.0 as usize }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActiveSlot {
    pub side: Side,
    pub index: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetLoc(i8);
impl TargetLoc {
    pub const AUTO: Self = Self(0);
    pub fn new<const ACTIVE: usize>(location: i8) -> Result<Self, PositionError> {
        if ![1, 2].contains(&ACTIVE) || location.unsigned_abs() as usize > ACTIVE {
            Err(PositionError::InvalidTarget)
        } else { Ok(Self(location)) }
    }
    pub const fn value(self) -> i8 { self.0 }

    pub fn resolve(self, source: Side) -> Option<ActiveSlot> {
        if self.0 == 0 { None } else {
            Some(ActiveSlot {side: if self.0 > 0 { source.foe() } else { source }, index: self.0.unsigned_abs() - 1})
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TargetKind {
    Normal,
    RandomNormal,
    Scripted,
    AdjacentAlly,
    AdjacentAllyOrSelf,
    AdjacentFoe,
    Any,
    Automatic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionError { InvalidFormat, InvalidMember, InvalidSlot, InvalidTarget, InvalidSelection, AlreadySelected, NotSelected, AlreadyActive }

pub fn valid_target<const ACTIVE: usize>(source: ActiveSlot, location: TargetLoc, kind: TargetKind) -> bool {
    if ![1, 2].contains(&ACTIVE) || source.index as usize >= ACTIVE || location.0.unsigned_abs() as usize > ACTIVE { return false; }
    if location.0 == 0 { return true; }
    let self_location = -(source.index as i8 + 1);
    let is_self = self_location == location.0;
    let is_foe = location.0 > 0;
    let across = -(ACTIVE as i8 + 1 - location.0);
    let adjacent = if is_foe { (across - self_location).abs() <= 1 } else { (location.0 - self_location).abs() == 1 };
    match kind {
        TargetKind::Normal | TargetKind::RandomNormal | TargetKind::Scripted => adjacent,
        TargetKind::AdjacentAlly => adjacent && !is_foe,
        TargetKind::AdjacentAllyOrSelf => (adjacent && !is_foe) || is_self,
        TargetKind::AdjacentFoe => adjacent && is_foe,
        TargetKind::Any => !is_self,
        TargetKind::Automatic => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Roster<const ACTIVE: usize> {
    picked: u8,
    active: [Option<MemberId>; ACTIVE],
    selected: bool,
}

impl<const ACTIVE: usize> Roster<ACTIVE> {
    pub fn new() -> Result<Self, PositionError> {
        if ![1, 2].contains(&ACTIVE) { return Err(PositionError::InvalidFormat); }
        Ok(Self { picked: 0, active: [None; ACTIVE], selected: false })
    }
    pub fn select(&mut self, members: &[MemberId]) -> Result<(), PositionError> {
        if self.selected { return Err(PositionError::AlreadySelected); }
        if members.len() != if ACTIVE == 1 { 3 } else { 4 } { return Err(PositionError::InvalidSelection); }
        let mut picked = 0;
        for member in members {
            let bit = 1 << member.0;
            if picked & bit != 0 { return Err(PositionError::InvalidSelection); }
            picked |= bit;
        }
        // Validate the whole selection before mutating shared state.
        self.picked = picked;
        for (index, member) in members.iter().take(ACTIVE).enumerate() { self.active[index] = Some(*member); }
        self.selected = true;
        Ok(())
    }
    pub fn contains(&self, member: MemberId) -> bool { self.picked & (1 << member.0) != 0 }
    pub fn active(&self, slot: u8) -> Result<Option<MemberId>, PositionError> {
        self.active.get(slot as usize).copied().ok_or(PositionError::InvalidSlot)
    }
    pub fn switch(&mut self, slot: u8, incoming: MemberId) -> Result<MemberId, PositionError> {
        if slot as usize >= ACTIVE { return Err(PositionError::InvalidSlot); }
        if !self.contains(incoming) { return Err(PositionError::NotSelected); }
        if self.active.contains(&Some(incoming)) { return Err(PositionError::AlreadyActive); }
        let outgoing = self.active[slot as usize].ok_or(PositionError::NotSelected)?;
        self.active[slot as usize] = Some(incoming);
        Ok(outgoing)
    }
}
