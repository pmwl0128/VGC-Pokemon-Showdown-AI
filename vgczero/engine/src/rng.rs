//! Battle randomness. `Rng::new` is xoshiro256** for training and search; `Rng::sodium`
//! reproduces the pinned Showdown SodiumRNG draw for draw (compatibility mode, used by the
//! exact parity harness). Both map every draw exactly like Showdown's `PRNG.random`, so the
//! battle code and its sequence of draws are the same in either mode; only the source differs.
//!
//! With the `trace` feature each draw is appended to a thread-local log (see `trace`).

use quark_drive_core::rng::SodiumRng;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Rng {
    Fast { s: [u64; 4] },
    Sodium(SodiumRng),
}

#[inline]
fn splitmix64(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[inline]
fn xoshiro(s: &mut [u64; 4]) -> u64 {
    let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
    let t = s[1] << 17;
    s[2] ^= s[0];
    s[3] ^= s[1];
    s[1] ^= s[2];
    s[0] ^= s[3];
    s[2] ^= t;
    s[3] = s[3].rotate_left(45);
    result
}

impl Rng {
    pub fn new(seed: u64) -> Rng {
        let mut x = seed;
        Rng::Fast { s: [splitmix64(&mut x), splitmix64(&mut x), splitmix64(&mut x), splitmix64(&mut x)] }
    }

    /// Showdown-compatible stream from a 32-byte sodium seed.
    pub fn sodium(seed: [u8; 32]) -> Rng {
        Rng::Sodium(SodiumRng::new(seed))
    }

    /// Parse Showdown's `sodium,<64 hex>` seed string.
    pub fn from_showdown_seed(seed: &str) -> Result<Rng, String> {
        let hex = seed.strip_prefix("sodium,").ok_or_else(|| format!("expected a sodium seed, got {seed:?}"))?;
        SodiumRng::from_hex(hex).map(Rng::Sodium).map_err(|e| e.to_string())
    }

    /// Showdown's `prng.getSeed()` for the compatibility stream.
    pub fn showdown_seed(&self) -> Option<String> {
        match self {
            Rng::Sodium(s) => Some(format!("sodium,{}", s.seed_hex())),
            Rng::Fast { .. } => None,
        }
    }

    /// One raw 32-bit draw: Showdown's `rng.next()`.
    #[inline]
    fn next_u32(&mut self) -> u32 {
        match self {
            Rng::Fast { s } => (xoshiro(s) >> 32) as u32,
            Rng::Sodium(r) => r.next_u32(),
        }
    }

    /// Harness use only (seeds, forks); never called by battle rules.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        match self {
            Rng::Fast { s } => xoshiro(s),
            Rng::Sodium(r) => ((r.next_u32() as u64) << 32) | r.next_u32() as u64,
        }
    }

    /// Uniform integer in [0, n): Showdown's `random(n)` = floor(next * n / 2^32).
    #[inline]
    #[cfg_attr(feature = "trace", track_caller)]
    pub fn below(&mut self, n: u32) -> u32 {
        crate::prof!("rng.below");
        debug_assert!(n > 0);
        let v = ((self.next_u32() as u64 * n as u64) >> 32) as u32;
        trace::record(0, n, v);
        v
    }

    /// Uniform integer in [lo, hi): Showdown's `random(lo, hi)`.
    #[inline]
    #[cfg_attr(feature = "trace", track_caller)]
    pub fn range(&mut self, lo: u32, hi: u32) -> u32 {
        crate::prof!("rng.range");
        debug_assert!(hi > lo);
        let v = lo + ((self.next_u32() as u64 * (hi - lo) as u64) >> 32) as u32;
        trace::record(lo, hi, v);
        v
    }

    /// True with probability num/den: Showdown's `randomChance` (= `random(den) < num`).
    #[inline]
    #[cfg_attr(feature = "trace", track_caller)]
    pub fn chance(&mut self, num: u32, den: u32) -> bool {
        self.below(den) < num
    }

    /// Showdown's `shuffle(items, start, end)` (Fisher-Yates from the front).
    #[inline]
    #[cfg_attr(feature = "trace", track_caller)]
    pub fn shuffle<T>(&mut self, items: &mut [T], mut start: usize, end: usize) {
        while start + 1 < end {
            let next = self.range(start as u32, end as u32) as usize;
            if next != start {
                items.swap(start, next);
            }
            start += 1;
        }
    }

    /// Harness use only (exploration noise, sampling); never called by battle rules.
    #[inline]
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Derive an independent fast stream (for search worlds, env resets).
    pub fn fork(&mut self) -> Rng {
        Rng::new(self.next_u64())
    }
}

/// Showdown's `speedSort`: a selection sort that moves each group of equal elements into
/// place in index order and Fisher-Yates shuffles the group with the battle RNG.
/// `cmp(a, b) == Less` means `a` goes first. Consumes no randomness without ties.
#[cfg_attr(feature = "trace", track_caller)]
pub fn speed_sort<T>(rng: &mut Rng, list: &mut [T], cmp: impl Fn(&T, &T) -> std::cmp::Ordering) {
    use std::cmp::Ordering;
    crate::prof!("speed_sort");
    const MAX: usize = 64;
    let n = list.len();
    assert!(n <= MAX, "speed_sort: {n} elements exceed the fixed capacity");
    let mut sorted = 0;
    while sorted + 1 < n {
        let mut best = sorted;
        let mut group_bits = 0u64;
        let mut count = 1;
        for i in sorted + 1..n {
            match cmp(&list[best], &list[i]) {
                Ordering::Less => {}
                Ordering::Greater => {
                    best = i;
                    group_bits = 0;
                    count = 1;
                }
                Ordering::Equal => {
                    if count == 1 {
                        group_bits = 1u64 << best;
                    }
                    group_bits |= 1u64 << i;
                    count += 1;
                }
            }
        }
        if count == 1 {
            if best != sorted {
                list.swap(sorted, best);
            }
        } else {
            for k in 0..count {
                let index = group_bits.trailing_zeros() as usize;
                group_bits &= group_bits - 1;
                if index != sorted + k {
                    list.swap(sorted + k, index);
                }
            }
        }
        if count > 1 {
            rng.shuffle(list, sorted, sorted + count);
        }
        sorted += count;
    }
}

/// speedSort under the patched oracle (adapters/showdown/tie_policy.cjs): a tie group for
/// which `artificial` holds (one holder, or holders without speed) takes the fixed order
/// `fixed` and draws nothing; any other tie is shuffled as in `speed_sort`.
#[cfg_attr(feature = "trace", track_caller)]
pub fn speed_sort_with<T>(
    rng: &mut Rng,
    list: &mut [T],
    cmp: impl Fn(&T, &T) -> std::cmp::Ordering,
    artificial: impl Fn(&[T]) -> bool,
    fixed: impl Fn(&T, &T) -> std::cmp::Ordering,
) {
    use std::cmp::Ordering;
    crate::prof!("speed_sort_with");
    const MAX: usize = 64;
    let n = list.len();
    assert!(n <= MAX, "speed_sort_with: {n} elements exceed the fixed capacity");
    let mut sorted = 0;
    while sorted + 1 < n {
        let mut best = sorted;
        let mut group_bits = 0u64;
        let mut count = 1;
        for i in sorted + 1..n {
            match cmp(&list[best], &list[i]) {
                Ordering::Less => {}
                Ordering::Greater => {
                    best = i;
                    group_bits = 0;
                    count = 1;
                }
                Ordering::Equal => {
                    if count == 1 {
                        group_bits = 1u64 << best;
                    }
                    group_bits |= 1u64 << i;
                    count += 1;
                }
            }
        }
        if count == 1 {
            if best != sorted {
                list.swap(sorted, best);
            }
        } else {
            for k in 0..count {
                let index = group_bits.trailing_zeros() as usize;
                group_bits &= group_bits - 1;
                if index != sorted + k {
                    list.swap(sorted + k, index);
                }
            }
        }
        if count > 1 {
            let group = &mut list[sorted..sorted + count];
            if artificial(group) {
                group.sort_by(&fixed);
            } else {
                rng.shuffle(list, sorted, sorted + count);
            }
        }
        sorted += count;
    }
}

pub mod trace {
    //! Draw log for the exact parity harness: (lo, hi, value, engine call site) per draw,
    //! `lo == 0` for `random(n)` with `hi == n`. Compiled out without the `trace` feature.
    #[cfg(feature = "trace")]
    thread_local! {
        static LOG: std::cell::RefCell<Option<Vec<(u32, u32, u32, String)>>> = const { std::cell::RefCell::new(None) };
    }

    #[inline(always)]
    #[cfg_attr(feature = "trace", track_caller)]
    pub fn record(_lo: u32, _hi: u32, _value: u32) {
        #[cfg(feature = "trace")]
        {
            let site = std::panic::Location::caller();
            LOG.with(|l| {
                if let Some(v) = l.borrow_mut().as_mut() {
                    v.push((_lo, _hi, _value, format!("{}:{}", site.file().rsplit(['/', '\\']).next().unwrap_or(""), site.line())));
                }
            });
        }
    }

    /// Start recording on this thread (clears any previous log).
    #[cfg(feature = "trace")]
    pub fn start() {
        LOG.with(|l| *l.borrow_mut() = Some(Vec::new()));
    }

    /// Stop recording and return the draws since `start`.
    #[cfg(feature = "trace")]
    pub fn take() -> Vec<(u32, u32, u32, String)> {
        LOG.with(|l| l.borrow_mut().take().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compare identities and RNG state with the direct Showdown selection algorithm,
    /// including displaced equal elements and artificial groups within mixed sorts.
    #[test]
    fn bitset_sort_preserves_selection_groups_and_draws() {
        fn reference(rng: &mut Rng, list: &mut [(u8, usize)], fixed: bool) {
            let mut sorted = 0;
            while sorted + 1 < list.len() {
                let mut next = vec![sorted];
                for i in sorted + 1..list.len() {
                    match list[next[0]].0.cmp(&list[i].0) {
                        std::cmp::Ordering::Less => {}
                        std::cmp::Ordering::Greater => next = vec![i],
                        std::cmp::Ordering::Equal => next.push(i),
                    }
                }
                for (k, &i) in next.iter().enumerate() {
                    list.swap(sorted + k, i);
                }
                let end = sorted + next.len();
                if fixed && list[sorted].0 == 1 {
                    list[sorted..end].sort_by_key(|x| x.1);
                } else {
                    rng.shuffle(list, sorted, end);
                }
                sorted = end;
            }
        }
        for n in 0..=7 {
            for code in 0..3usize.pow(n) {
                let mut digits = code;
                let input: Vec<_> = (0..n as usize).map(|i| {
                    let key = (digits % 3) as u8;
                    digits /= 3;
                    (key, i)
                }).collect();
                for fixed in [false, true] {
                    let mut expected = input.clone();
                    let mut actual = input.clone();
                    let mut want_rng = Rng::new(code as u64);
                    let mut got_rng = want_rng;
                    reference(&mut want_rng, &mut expected, fixed);
                    if fixed {
                        speed_sort_with(&mut got_rng, &mut actual, |a, b| a.0.cmp(&b.0),
                            |g| g[0].0 == 1, |a, b| a.1.cmp(&b.1));
                    } else {
                        speed_sort(&mut got_rng, &mut actual, |a, b| a.0.cmp(&b.0));
                    }
                    assert_eq!(actual, expected, "n={n} code={code} fixed={fixed}");
                    assert_eq!(got_rng, want_rng, "n={n} code={code} fixed={fixed}");
                }
            }
        }
        // The last representable bit must not overflow or truncate the group.
        let mut actual = [(0u8, 0usize); 64];
        for (i, v) in actual.iter_mut().enumerate() { *v = ((i % 3) as u8, i); }
        let mut expected = actual;
        let mut a = Rng::new(42);
        let mut b = a;
        reference(&mut a, &mut expected, false);
        speed_sort(&mut b, &mut actual, |a, b| a.0.cmp(&b.0));
        assert_eq!(actual, expected);
        assert_eq!(a, b);
    }

    #[test]
    fn speed_sort_matches_showdown_selection_and_shuffle() {
        // Without ties: a stable selection sort and no draws.
        let mut rng = Rng::new(1);
        let before = rng;
        let mut v = [3, 1, 2];
        speed_sort(&mut rng, &mut v, |a: &i32, b: &i32| a.cmp(b));
        assert_eq!(v, [1, 2, 3]);
        assert_eq!(rng, before);
        // A tie group of three consumes two draws: random(0,3) then random(1,3).
        let mut rng = Rng::new(7);
        let mut expect = rng;
        let mut v = [(5, 'a'), (1, 'x'), (5, 'b'), (5, 'c')];
        speed_sort(&mut rng, &mut v, |a: &(i32, char), b: &(i32, char)| a.0.cmp(&b.0));
        assert_eq!(v[0], (1, 'x'));
        let mut group = ['a', 'b', 'c'];
        expect.shuffle(&mut group, 0, 3);
        assert_eq!([v[1].1, v[2].1, v[3].1], group);
        assert_eq!(rng, expect);
    }

    #[test]
    fn sodium_stream_maps_draws_like_showdown_random() {
        // random(n) and random(lo, hi) both scale one 32-bit draw.
        let mut a = Rng::sodium([3; 32]);
        let mut b = a;
        let x = a.below(16);
        let y = b.range(0, 16);
        assert_eq!(x, y);
        assert_eq!(a, b);
        assert!(Rng::from_showdown_seed(&a.showdown_seed().unwrap()).is_ok());
    }
}
