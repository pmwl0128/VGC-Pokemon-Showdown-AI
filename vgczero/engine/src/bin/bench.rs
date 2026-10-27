//! Random self-play benchmark and team coverage report.
//!
//!   cargo run --release --bin bench -- [games] [--all-teams] [--clone-step STATES] [--unchecked]
//!
//! `check` folds every game's turns, winner and final battle RNG state: a change that is meant
//! to be behaviour-preserving must leave it unchanged. `--clone-step` collects STATES decision
//! points from random games, then times copy + one step to the next decision from each of
//! them with pre-drawn random legal choices (the copy/restore + advance metric of PLAN §5).
//! `--unchecked` steps with `step_unchecked` (the caller vouches for the choices, as a search
//! or a masked policy can): same transitions and `check`, without the validation cost.

use std::time::Instant;

use vgczero_engine::actions::Choice;
use vgczero_engine::battle::{Battle, BattleConfig};
use vgczero_engine::rng::Rng;
use vgczero_engine::{load_dex, load_teams};

fn random_choice(b: &Battle, side: usize, rng: &mut Rng) -> Choice {
    b.random_choice(side, rng)
}

#[inline]
fn fold(h: u64, x: u64) -> u64 {
    (h ^ x).wrapping_mul(0x100_0000_01b3).rotate_left(17)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let games: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2000);
    let all = args.iter().any(|a| a == "--all-teams");
    let unchecked = args.iter().any(|a| a == "--unchecked");
    let mode = if unchecked { " [unchecked]" } else { "" };
    let clone_step: Option<usize> =
        args.iter().position(|a| a == "--clone-step").and_then(|i| args.get(i + 1)).and_then(|s| s.parse().ok());
    load_dex(&vgczero_engine::data_path("dex.json.gz")).unwrap();
    let teams = load_teams(&vgczero_engine::data_path("teams.json.gz")).unwrap();
    let supported: Vec<_> = teams.iter().filter(|t| t.supported()).collect();
    println!("teams: {} total, {} fully supported ({:.1}%)", teams.len(), supported.len(), 100.0 * supported.len() as f64 / teams.len() as f64);
    let mut reasons = std::collections::HashMap::<String, usize>::new();
    for t in &teams {
        for r in &t.unsupported {
            *reasons.entry(r.clone()).or_default() += 1;
        }
    }
    let mut rs: Vec<_> = reasons.into_iter().collect();
    rs.sort_by(|a, b| b.1.cmp(&a.1));
    println!("top blockers (teams affected):");
    for (r, c) in rs.iter().take(25) {
        println!("  {c:5}  {r}");
    }
    let pool: Vec<_> = if all { teams.iter().collect() } else { supported };
    if pool.is_empty() {
        return;
    }
    if let Some(n) = clone_step {
        bench_clone_step(&pool, n, games, unchecked);
        return;
    }
    let mut rng = Rng::new(12345);
    let t0 = Instant::now();
    let (mut turns, mut decisions, mut wins) = (0u64, 0u64, [0u64; 3]);
    let mut errors = 0;
    let mut check = 0u64;
    for g in 0..games {
        let a = pool[rng.below(pool.len() as u32) as usize];
        let bteam = pool[rng.below(pool.len() as u32) as usize];
        let mut b = Battle::new([a, bteam], g as u64, BattleConfig::default());
        let mut steps = 0;
        while !b.ended() && steps < 1000 {
            let c = [random_choice(&b, 0, &mut rng), random_choice(&b, 1, &mut rng)];
            if unchecked {
                b.step_unchecked(c);
            } else if let Err(e) = b.step(c) {
                errors += 1;
                if errors < 5 {
                    eprintln!("game {g}: {e}\n{}", b.summary());
                }
                break;
            }
            steps += 1;
            decisions += 1;
        }
        turns += b.turn as u64;
        if let Some(w) = b.winner {
            wins[w as usize] += 1;
        }
        check = fold(check, b.turn as u64 | (b.winner.unwrap_or(9) as u64) << 16);
        check = fold(check, b.rng.next_u64());
    }
    let dt = t0.elapsed().as_secs_f64();
    println!(
        "{games} games in {dt:.2}s: {:.0} games/s, {:.0} decision steps/s, {:.1} turns/game, wins {:?}, errors {errors}, check {check:016x}{mode}",
        games as f64 / dt,
        decisions as f64 / dt,
        turns as f64 / games as f64,
        wins
    );
    print_prof(decisions);
}

/// With `--features prof`: calls and inclusive cycles per decision step for every scope.
fn print_prof(_steps: u64) {
    #[cfg(feature = "prof")]
    {
        let mut r = vgczero_engine::prof::report();
        r.sort_by(|a, b| b.2.cmp(&a.2));
        let steps = _steps.max(1) as f64;
        println!("{:>28} {:>10} {:>12} {:>10}", "scope", "calls/step", "cycles/step", "cyc/call");
        for (name, calls, cyc) in r {
            println!("{name:>28} {:>10.2} {:>12.0} {:>10.0}", calls as f64 / steps, cyc as f64 / steps, cyc as f64 / calls.max(1) as f64);
        }
    }
}

/// Copy + step from `n` collected decision points, each with `K` pre-drawn random choices,
/// repeated `reps` times.
fn bench_clone_step(pool: &[&vgczero_engine::TeamSpec], n: usize, reps: usize, unchecked: bool) {
    let mode = if unchecked { " [unchecked]" } else { "" };
    const K: usize = 4;
    let mut rng = Rng::new(777);
    let mut states: Vec<(Battle, [[Choice; 2]; K])> = Vec::with_capacity(n);
    let mut g = 0u64;
    while states.len() < n {
        let a = pool[rng.below(pool.len() as u32) as usize];
        let bteam = pool[rng.below(pool.len() as u32) as usize];
        let mut b = Battle::new([a, bteam], g, BattleConfig::default());
        g += 1;
        let mut steps = 0;
        while !b.ended() && steps < 1000 && states.len() < n {
            // Every decision after team preview, sampled at 1 in 2.
            if steps > 0 && rng.below(2) == 0 {
                let mut cs = [[Choice::default(); 2]; K];
                for c in cs.iter_mut() {
                    *c = [random_choice(&b, 0, &mut rng), random_choice(&b, 1, &mut rng)];
                }
                states.push((b.clone(), cs));
            }
            let c = [random_choice(&b, 0, &mut rng), random_choice(&b, 1, &mut rng)];
            if b.step(c).is_err() {
                break;
            }
            steps += 1;
        }
    }
    let reps = reps.max(1);
    #[cfg(feature = "prof")]
    vgczero_engine::prof::reset();
    let mut check = 0u64;
    let mut errors = 0u64;
    let t0 = Instant::now();
    for r in 0..reps {
        for (s, cs) in &states {
            let mut b = s.clone();
            if unchecked {
                b.step_unchecked(cs[r % K]);
            } else if b.step(cs[r % K]).is_err() {
                errors += 1;
            }
            check = fold(check, b.rng.next_u64() ^ b.turn as u64);
        }
    }
    let dt = t0.elapsed().as_secs_f64();
    let steps = (reps * states.len()) as f64;
    println!(
        "clone-step: {} states x {reps}: {:.0} ns/step, {:.0} steps/s, battle {} bytes, errors {errors}, check {check:016x}{mode}",
        states.len(),
        dt * 1e9 / steps,
        steps / dt,
        std::mem::size_of::<Battle>()
    );
    print_prof(reps as u64 * states.len() as u64);
}
