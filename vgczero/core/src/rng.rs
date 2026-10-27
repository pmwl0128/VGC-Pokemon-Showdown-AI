//! ChaCha20 block computation follows RFC 8439. The fixed nonce, rekeying and word byte
//! order implement the pinned Showdown SodiumRNG contract; no external cipher runtime.
use std::fmt::{self, Write};

const NONCE: &[u8; 12] = b"LibsodiumDRG";
const UNIT: f64 = 4_294_967_296.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SodiumRng {
    seed: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RngError {
    InvalidSeed,
    InvalidShuffleBounds,
}

impl fmt::Display for RngError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidSeed => "expected exactly 64 lowercase hexadecimal characters",
            Self::InvalidShuffleBounds => "invalid shuffle interval",
        })
    }
}
impl std::error::Error for RngError {}

impl SodiumRng {
    pub const fn new(seed: [u8; 32]) -> Self {
        Self { seed }
    }

    pub fn from_hex(text: &str) -> Result<Self, RngError> {
        if text.len() != 64 {
            return Err(RngError::InvalidSeed);
        }
        let mut seed = [0u8; 32];
        for (index, pair) in text.as_bytes().chunks_exact(2).enumerate() {
            let digit = |byte| match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                _ => Err(RngError::InvalidSeed),
            };
            seed[index] = (digit(pair[0])? << 4) | digit(pair[1])?;
        }
        Ok(Self::new(seed))
    }

    pub const fn state(&self) -> [u8; 32] {
        self.seed
    }

    pub fn seed_hex(&self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.seed {
            write!(&mut output, "{byte:02x}").expect("String formatting cannot fail");
        }
        output
    }

    pub fn next_u32(&mut self) -> u32 {
        let block = block(&self.seed);
        self.seed.copy_from_slice(&block[..32]);
        u32::from_be_bytes(block[32..36].try_into().expect("fixed block slice"))
    }

    /// Integer-argument PRNG.random compatibility, including its zero-bound behavior.
    /// f64 operations deliberately preserve JavaScript's mapping and rounding order.
    pub fn random(&mut self, from: Option<i64>, to: Option<i64>) -> f64 {
        let word = self.next_u32() as f64;
        match from {
            None => word / UNIT,
            Some(from) => {
                let from = from as f64;
                match to {
                    Some(to) if to != 0 => (word * (to as f64 - from) / UNIT).floor() + from,
                    _ => (word * from / UNIT).floor(),
                }
            }
        }
    }

    pub fn chance(&mut self, numerator: u32, denominator: u32) -> bool {
        self.random(Some(denominator.into()), None) < numerator.into()
    }

    pub fn shuffle<T>(&mut self, values: &mut [T], start: usize, end: usize) -> Result<(), RngError> {
        if start > end || end > values.len() || end > i64::MAX as usize {
            return Err(RngError::InvalidShuffleBounds);
        }
        let mut position = start;
        while position < end.saturating_sub(1) {
            let next = self.random(Some(position as i64), Some(end as i64)) as usize;
            values.swap(position, next);
            position += 1;
        }
        Ok(())
    }
}

fn quarter(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = (state[d] ^ state[a]).rotate_left(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_left(12);
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = (state[d] ^ state[a]).rotate_left(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_left(7);
}

fn block(key: &[u8; 32]) -> [u8; 64] {
    let mut initial = [0u32; 16];
    initial[..4].copy_from_slice(&[0x61707865, 0x3320646e, 0x79622d32, 0x6b206574]);
    for (index, word) in key.chunks_exact(4).enumerate() {
        initial[4 + index] = u32::from_le_bytes(word.try_into().expect("key word"));
    }
    // Every rekeyed stream starts at counter zero and consumes only its first 36 bytes.
    for (index, word) in NONCE.chunks_exact(4).enumerate() {
        initial[13 + index] = u32::from_le_bytes(word.try_into().expect("nonce word"));
    }
    let mut state = initial;
    for _ in 0..10 {
        quarter(&mut state, 0, 4, 8, 12);
        quarter(&mut state, 1, 5, 9, 13);
        quarter(&mut state, 2, 6, 10, 14);
        quarter(&mut state, 3, 7, 11, 15);
        quarter(&mut state, 0, 5, 10, 15);
        quarter(&mut state, 1, 6, 11, 12);
        quarter(&mut state, 2, 7, 8, 13);
        quarter(&mut state, 3, 4, 9, 14);
    }
    let mut output = [0u8; 64];
    for (index, word) in state.iter().enumerate() {
        output[index * 4..index * 4 + 4].copy_from_slice(&word.wrapping_add(initial[index]).to_le_bytes());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_vectors_cover_words_ranges_and_successor_seeds() {
        let vectors = include_str!("../../../../tests/fixtures/fast_engine/native_rng.tsv");
        let mut count = 0;
        for line in vectors.lines().filter(|line| !line.starts_with('#')) {
            let fields: Vec<_> = line.split('\t').collect();
            let mut rng = SodiumRng::from_hex(fields[0]).unwrap();
            let mut raw = rng;
            let optional = |value: &str| if value == "-" { None } else { Some(value.parse::<i64>().unwrap()) };
            assert_eq!(raw.next_u32(), fields[4].parse::<u32>().unwrap());
            assert_eq!(rng.random(optional(fields[1]), optional(fields[2])), fields[3].parse::<f64>().unwrap());
            assert_eq!(rng.seed_hex(), fields[5]);
            assert_eq!(raw.state(), rng.state());
            count += 1;
        }
        assert_eq!(count, 640);
    }

    #[test]
    fn copies_fork_without_aliasing_and_empty_shuffles_consume_no_draws() {
        let original = SodiumRng::new([0; 32]);
        let mut left = original;
        let mut right = original;
        assert_eq!(left.next_u32(), right.next_u32());
        assert_eq!(original.state(), [0; 32]);
        let before = left;
        left.shuffle::<u8>(&mut [], 0, 0).unwrap();
        left.shuffle(&mut [1], 0, 1).unwrap();
        assert_eq!(left, before);
        assert_eq!(left.shuffle(&mut [0, 1], 2, 1), Err(RngError::InvalidShuffleBounds));
        assert_eq!(left, before);
        assert_eq!(SodiumRng::from_hex("bad"), Err(RngError::InvalidSeed));
        assert_eq!(SodiumRng::from_hex(&"G".repeat(64)), Err(RngError::InvalidSeed));
    }

    #[test]
    fn shuffle_vectors_preserve_permutation_and_exact_consumption() {
        let vectors = include_str!("../../../../tests/fixtures/fast_engine/native_rng_shuffle.tsv");
        for line in vectors.lines().filter(|line| !line.starts_with('#')) {
            let fields: Vec<_> = line.split('\t').collect();
            let parse = |text: &str| -> Vec<u32> {
                if text == "-" { vec![] } else { text.split(',').map(|value| value.parse().unwrap()).collect() }
            };
            let mut rng = SodiumRng::from_hex(fields[0]).unwrap();
            let mut values = parse(fields[3]);
            rng.shuffle(&mut values, fields[1].parse().unwrap(), fields[2].parse().unwrap()).unwrap();
            assert_eq!(values, parse(fields[4]));
            assert_eq!(rng.seed_hex(), fields[5]);
        }
    }

    #[test]
    fn chance_vectors_consume_one_draw_even_at_probability_boundaries() {
        let vectors = include_str!("../../../../tests/fixtures/fast_engine/native_rng_chance.tsv");
        for line in vectors.lines().filter(|line| !line.starts_with('#')) {
            let fields: Vec<_> = line.split('\t').collect();
            let mut rng = SodiumRng::from_hex(fields[0]).unwrap();
            assert_eq!(rng.chance(fields[1].parse().unwrap(), fields[2].parse().unwrap()), fields[3].parse::<bool>().unwrap());
            assert_eq!(rng.seed_hex(), fields[4]);
        }
    }
}
