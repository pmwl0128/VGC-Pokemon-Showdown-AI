use vgczero_engine::actions::Choice;
use vgczero_engine::battle::{Battle, BattleConfig};
use vgczero_engine::rng::Rng;
use vgczero_engine::{load_dex, load_teams, TeamSpec};

pub fn setup() -> Vec<TeamSpec> {
    load_dex(&vgczero_engine::data_path("dex.json.gz")).unwrap();
    load_teams(&vgczero_engine::data_path("teams.json.gz")).unwrap()
}

fn random_choice(b: &Battle, side: usize, rng: &mut Rng) -> Choice {
    b.random_choice(side, rng)
}

#[test]
fn one_logged_game() {
    let teams = setup();
    let sup: Vec<_> = teams.iter().filter(|t| t.supported()).collect();
    let cfg = BattleConfig { log: true, ..Default::default() };
    let mut b = Battle::new([sup[0], sup[1]], 7, cfg);
    let mut rng = Rng::new(1);
    for step in 0..40 {
        if b.ended() { break; }
        let c = [random_choice(&b, 0, &mut rng), random_choice(&b, 1, &mut rng)];
        println!("step {step} phase {:?} req {:?} {:?} choice {:?}", b.phase, b.request(0), b.request(1), c);
        b.step(c).unwrap();
    }
    for l in b.log() { println!("{l}"); }
    println!("{}", b.summary());
}

/// Per-slot sampling from `legal_mask` (only avoiding the same switch target
/// twice and two Mega Evolutions), as an RL policy does, never makes `step`
/// fail: joint constraints the masks cannot express are completed by the
/// engine (e.g. which slot gets the only replacement).
#[test]
fn per_slot_mask_sampling_is_always_accepted() {
    use vgczero_engine::actions::{decode, SlotAction, PASS};
    use vgczero_engine::battle::RequestKind;
    let teams = setup();
    let sup: Vec<_> = teams.iter().filter(|t| t.supported()).collect();
    let mut rng = Rng::new(3);
    for g in 0..300u64 {
        let ta = sup[rng.below(sup.len() as u32) as usize];
        let tb = sup[rng.below(sup.len() as u32) as usize];
        let mut b = Battle::new([ta, tb], g, BattleConfig::default());
        while !b.ended() {
            let mut c = [Choice::default(), Choice::default()];
            for s in 0..2 {
                if b.request(s).kind == RequestKind::TeamPreview {
                    c[s] = Choice::preview(rng.below(90) as u8);
                    continue;
                }
                let (mut used, mut mega) = ([false; 6], false);
                let mut slots = [PASS; 2];
                for slot in 0..2 {
                    let mask = b.legal_mask(s, slot);
                    let opts: Vec<u8> = (0..31u8)
                        .filter(|&a| mask & (1 << a) != 0)
                        .filter(|&a| match decode(a) {
                            SlotAction::Switch(t) => !used[t as usize],
                            SlotAction::Move { mega: m, .. } => !(m && mega),
                            SlotAction::Pass => true,
                        })
                        .collect();
                    let a = if opts.is_empty() { PASS } else { opts[rng.below(opts.len() as u32) as usize] };
                    match decode(a) {
                        SlotAction::Switch(t) => used[t as usize] = true,
                        SlotAction::Move { mega: true, .. } => mega = true,
                        _ => {}
                    }
                    slots[slot] = a;
                }
                c[s] = Choice::slots(slots[0], slots[1]);
            }
            b.step(c).unwrap_or_else(|e| panic!("game {g}: {}\n{}", e.0, b.summary()));
        }
    }
}

/// `action_legal` (step's validation) agrees with `legal_mask` for every slot action at
/// every decision point of random games.
#[test]
fn action_legal_matches_the_mask() {
    let teams = setup();
    let sup: Vec<_> = teams.iter().filter(|t| t.supported()).collect();
    let mut rng = Rng::new(11);
    let mut checked = 0u64;
    for g in 0..400u64 {
        let ta = sup[rng.below(sup.len() as u32) as usize];
        let tb = sup[rng.below(sup.len() as u32) as usize];
        let mut b = Battle::new([ta, tb], g, BattleConfig::default());
        while !b.ended() {
            for side in 0..2 {
                for slot in 0..2 {
                    let mask = b.legal_mask(side, slot);
                    for a in 0..31u8 {
                        assert_eq!(b.action_legal(side, slot, a), mask & (1 << a) != 0, "game {g} side {side} slot {slot} action {a}
{}", b.summary());
                        checked += 1;
                    }
                }
            }
            let c = [random_choice(&b, 0, &mut rng), random_choice(&b, 1, &mut rng)];
            b.step(c).unwrap();
        }
    }
    assert!(checked > 100_000);
}

fn reference_random_choice(b: &Battle, side: usize, rng: &mut Rng) -> Choice {
    use vgczero_engine::actions::{decode, Choice, SlotAction, N_PREVIEW_ACTIONS};
    use vgczero_engine::battle::{Phase, RequestKind};
    use vgczero_engine::actions::{PASS, switch_action};
    let r = b.request(side);
    match r.kind {
        RequestKind::TeamPreview => return Choice::preview(rng.below(N_PREVIEW_ACTIONS as u32) as u8),
        RequestKind::Wait => return Choice::default(),
        _ => {}
    }
    let mut c = Choice::slots(PASS, PASS);
    let mut used = [false; 6];
    let mut mega = false;
    let mut opts = [0u8; 31];
    for slot in 0..2 {
        let mask = b.legal_mask(side, slot);
        let mut n = 0;
        for a in 0..31u8 {
            if mask & (1 << a) == 0 {
                continue;
            }
            let ok = match decode(a) {
                SlotAction::Switch(t) => !used[t as usize],
                SlotAction::Move { mega: m, .. } => !(m && mega),
                SlotAction::Pass => true,
            };
            if ok {
                opts[n] = a;
                n += 1;
            }
        }
        let a = if n == 0 { PASS } else { opts[rng.below(n as u32) as usize] };
        match decode(a) {
            SlotAction::Switch(t) => used[t as usize] = true,
            SlotAction::Move { mega: true, .. } => mega = true,
            _ => {}
        }
        c.slots[slot] = a;
    }
    if matches!(b.phase, Phase::Switch { .. }) {
        // Fill passes until the required number of replacements switch in.
        let mut switches = c.slots.iter().filter(|&&a| matches!(decode(a), SlotAction::Switch(_))).count();
        for slot in 0..2 {
            if switches >= b.forced_switches(side) {
                break;
            }
            if b.switch_slots[side][slot] && c.slots[slot] == PASS {
                if let Some(i) = (0..6).find(|&i| b.switch_target_ok(side, slot, i) && !used[i]) {
                    used[i] = true;
                    c.slots[slot] = switch_action(i);
                    switches += 1;
                }
            }
        }
    }
    c
}


/// Sampling the same ascending menu must preserve both the chosen action and every draw.
#[test]
fn random_choice_preserves_menu_order_and_draws() {
    let teams = setup();
    let sup: Vec<_> = teams.iter().filter(|t| t.supported()).collect();
    let mut policy = Rng::new(29);
    let mut checked = 0usize;
    for game in 0..120 {
        let a = sup[policy.below(sup.len() as u32) as usize];
        let b = sup[policy.below(sup.len() as u32) as usize];
        let mut battle = Battle::new([a, b], game, BattleConfig::default());
        while !battle.ended() {
            for side in 0..2 {
                for stream in [Rng::new(checked as u64), Rng::sodium([checked as u8; 32])] {
                    let mut actual_rng = stream;
                    let mut expected_rng = stream;
                    let actual = battle.random_choice(side, &mut actual_rng);
                    let expected = reference_random_choice(&battle, side, &mut expected_rng);
                    assert_eq!(actual, expected, "game {game} side {side}");
                    assert_eq!(actual_rng, expected_rng, "game {game} side {side}");
                    checked += 1;
                }
            }
            let choices = [battle.random_choice(0, &mut policy), battle.random_choice(1, &mut policy)];
            battle.step(choices).unwrap();
        }
    }
    assert!(checked > 4000);
}

/// `step_unchecked` makes the same transition as `step` for the choices `step` accepts,
/// in every phase (team preview, moves, switches), including the draws it takes.
#[test]
fn step_unchecked_matches_step() {
    let teams = setup();
    let sup: Vec<_> = teams.iter().filter(|t| t.supported()).collect();
    let mut policy = Rng::new(31);
    let mut steps = 0usize;
    for game in 0..150 {
        let a = sup[policy.below(sup.len() as u32) as usize];
        let b = sup[policy.below(sup.len() as u32) as usize];
        let mut checked = Battle::new([a, b], game, BattleConfig::default());
        let mut unchecked = checked.clone();
        while !checked.ended() {
            let c = [checked.random_choice(0, &mut policy), checked.random_choice(1, &mut policy)];
            checked.step(c).unwrap();
            unchecked.step_unchecked(c);
            assert_eq!(unchecked.summary(), checked.summary(), "game {game} step {steps}");
            assert_eq!(unchecked.rng, checked.rng, "game {game} step {steps}");
            assert_eq!((unchecked.turn, unchecked.phase, unchecked.winner), (checked.turn, checked.phase, checked.winner));
            steps += 1;
        }
    }
    assert!(steps > 1500);
}

/// Effect ranks (ties.rs) order like the id strings across moves, items, abilities and
/// conditions; equal ids share a rank, and the compiled whitelist answers on ranks.
#[test]
fn effect_ranks_follow_id_order() {
    use vgczero_engine::battle::ties::{Ev, Hk};
    use vgczero_engine::dex::{dex, Cond};
    setup();
    let d = dex();
    let mut pairs: Vec<(&str, u16)> = d
        .moves
        .iter()
        .map(|x| (x.id.as_str(), x.rank))
        .chain(d.items.iter().map(|x| (x.id.as_str(), x.rank)))
        .chain(d.abilities.iter().map(|x| (x.id.as_str(), x.rank)))
        .chain(Cond::IDS.iter().enumerate().map(|(k, id)| (*id, d.cond_rank[k])))
        .collect();
    pairs.sort();
    for w in pairs.windows(2) {
        assert_eq!(w[0].0.cmp(w[1].0), w[0].1.cmp(&w[1].1), "{:?} vs {:?}", w[0], w[1]);
    }
    assert_eq!(d.cr(Cond::Protect), d.mv(d.move_id("protect").unwrap()).rank);
    assert_eq!(d.cr(Cond::Leftovers), d.item(d.item_id("leftovers").unwrap()).rank);
    assert!(d.whitelist.contains(Ev::Residual, &[d.cr(Cond::Stall), d.cr(Cond::Protect)], Hk::Pokemon));
    assert!(!d.whitelist.contains(Ev::Residual, &[d.cr(Cond::Stall), d.cr(Cond::KingsShield)], Hk::Pokemon));
}
