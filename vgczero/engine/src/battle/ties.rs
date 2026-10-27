//! The verified tie whitelist of the patched oracle
//! (model_train/artifacts/evaluation/fast-engine-state-20261024/distribution/whitelist.verified.json,
//! applied by adapters/showdown/tie_policy.cjs). An artificial tie (one holder, or holders without
//! speed) in one of these categories keeps a fixed order and draws nothing; every other tie,
//! artificial or real, is shuffled. Categories are (event, effect ids sorted, holder kinds).
//!
//! The rule code never compares id strings: the dex ranks every move, item, ability and
//! condition id in id order (`Dex::cond_rank`, `MoveData::rank`, ...), a handler carries its
//! effect's rank, the fixed order of a whitelisted tie compares ranks, and `Whitelist` holds the
//! categories as integer keys. `tests` recomputes each category's hash against the whitelist file.

pub const WHITELIST: [(&str, &[&str], &str); 15] = [
    ("ModifyDamage", &["auroraveil", "auroraveil"], "Side"),
    ("ModifyDamage", &["lightscreen", "reflect"], "Side"),
    ("Residual", &["auroraveil", "auroraveil"], "Side"),
    ("Residual", &["banefulbunker", "stall"], "Pokemon"),
    ("Residual", &["flinch", "stall"], "Pokemon"),
    ("Residual", &["flinch", "twoturnmove"], "Pokemon"),
    ("Residual", &["helpinghand", "protect", "stall"], "Pokemon"),
    ("Residual", &["mustrecharge", "stall"], "Pokemon"),
    ("Residual", &["phantomforce", "stall", "twoturnmove"], "Pokemon"),
    ("Residual", &["phantomforce", "twoturnmove"], "Pokemon"),
    ("Residual", &["protect", "stall"], "Pokemon"),
    ("Residual", &["ragepowder", "stall"], "Pokemon"),
    ("Residual", &["spikyshield", "stall"], "Pokemon"),
    ("Residual", &["stall", "twoturnmove"], "Pokemon"),
    ("Residual", &["tailwind", "tailwind"], "Side"),
];

/// Events whose handler sorts can meet an artificial tie.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Ev {
    BasePower,
    ModifyAtk,
    ModifySpA,
    ModifyDamage,
    Residual,
    DisableMove,
    TrapPokemon,
}

impl Ev {
    fn from_name(s: &str) -> Option<Ev> {
        Some(match s {
            "BasePower" => Ev::BasePower,
            "ModifyAtk" => Ev::ModifyAtk,
            "ModifySpA" => Ev::ModifySpA,
            "ModifyDamage" => Ev::ModifyDamage,
            "Residual" => Ev::Residual,
            "DisableMove" => Ev::DisableMove,
            "TrapPokemon" => Ev::TrapPokemon,
            _ => return None,
        })
    }
}

/// Handler holder kinds in the order of their Showdown names ("Field" < "Pokemon" < "Side"),
/// the order the fixed sort of a whitelisted tie compares after the effect.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Hk {
    Field = 0,
    Pokemon = 1,
    Side = 2,
}

impl Hk {
    fn from_name(s: &str) -> Option<Hk> {
        Some(match s {
            "Field" => Hk::Field,
            "Pokemon" => Hk::Pokemon,
            "Side" => Hk::Side,
            _ => return None,
        })
    }
}

/// The whitelist as integer keys: event, holder kind and the sorted effect ranks of a category.
#[derive(Clone, Debug)]
pub struct Whitelist {
    keys: [u64; WHITELIST.len()],
}

impl Whitelist {
    /// The largest category has three effects: a longer tie group is never whitelisted.
    pub const MAX_EFFECTS: usize = 3;

    /// Compile the categories with `rank` (an id's position among all ranked ids).
    pub fn compile(rank: impl Fn(&str) -> u16) -> Whitelist {
        let mut keys = [0u64; WHITELIST.len()];
        for (k, (event, ids, holder)) in WHITELIST.iter().enumerate() {
            assert!(ids.len() <= Self::MAX_EFFECTS, "whitelist category wider than MAX_EFFECTS");
            let mut r = [u16::MAX; Self::MAX_EFFECTS];
            for (i, id) in ids.iter().enumerate() {
                r[i] = rank(id);
            }
            r.sort_unstable();
            keys[k] = Self::key(Ev::from_name(event).expect("whitelist event"), Hk::from_name(holder).expect("whitelist holder"), r);
        }
        Whitelist { keys }
    }

    #[inline]
    fn key(event: Ev, holder: Hk, sorted: [u16; Self::MAX_EFFECTS]) -> u64 {
        (event as u64) << 56 | (holder as u64) << 48 | (sorted[0] as u64) << 32 | (sorted[1] as u64) << 16 | sorted[2] as u64
    }

    /// Is an artificial tie of these effect ranks (any order) in a whitelisted category?
    #[inline]
    pub fn contains(&self, event: Ev, ranks: &[u16], holder: Hk) -> bool {
        if ranks.len() > Self::MAX_EFFECTS {
            return false;
        }
        // Three-element sorting network; the u16::MAX fillers of a shorter group sort last.
        let mut r = [u16::MAX; Self::MAX_EFFECTS];
        r[..ranks.len()].copy_from_slice(ranks);
        if r[0] > r[1] {
            r.swap(0, 1);
        }
        if r[1] > r[2] {
            r.swap(1, 2);
        }
        if r[0] > r[1] {
            r.swap(0, 1);
        }
        let key = Self::key(event, holder, r);
        self.keys.iter().any(|&k| k == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_matches_the_verified_oracle_categories() {
        // sha256 of the canonical JSON {"effects":[...],"event":...,"holders":[...]} (contracts.cjs hash).
        let path = concat!(env!("CARGO_MANIFEST_DIR"),
            "/../../../artifacts/evaluation/fast-engine-state-20261024/distribution/whitelist.verified.json");
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => {
                eprintln!("skipping: no whitelist receipt at {path} (the engine tree is outside the Quark Drive repository)");
                return;
            }
        };
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut want: Vec<String> = doc["categories"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        want.sort();
        let mut got: Vec<String> = WHITELIST
            .iter()
            .map(|(event, ids, holder)| {
                let effects = ids.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(",");
                let canon = format!("{{\"effects\":[{effects}],\"event\":\"{event}\",\"holders\":[\"{holder}\"]}}");
                sha256_hex(canon.as_bytes())
            })
            .collect();
        got.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn compiled_whitelist_matches_categories_by_rank() {
        // Any id-ordered ranking works; here the whitelist's own ids plus a stranger.
        let mut ids: Vec<&str> = WHITELIST.iter().flat_map(|(_, ids, _)| ids.iter().copied()).chain(["kingsshield"]).collect();
        ids.sort_unstable();
        ids.dedup();
        let rank = |s: &str| ids.binary_search(&s).unwrap() as u16;
        let wl = Whitelist::compile(rank);
        assert!(wl.contains(Ev::Residual, &[rank("stall"), rank("protect")], Hk::Pokemon));
        assert!(wl.contains(Ev::Residual, &[rank("twoturnmove"), rank("stall"), rank("phantomforce")], Hk::Pokemon));
        assert!(wl.contains(Ev::ModifyDamage, &[rank("reflect"), rank("lightscreen")], Hk::Side));
        assert!(!wl.contains(Ev::Residual, &[rank("kingsshield"), rank("stall")], Hk::Pokemon));
        assert!(!wl.contains(Ev::ModifyDamage, &[rank("stall"), rank("protect")], Hk::Pokemon));
        assert!(!wl.contains(Ev::Residual, &[rank("stall"), rank("protect")], Hk::Side));
        assert!(!wl.contains(Ev::Residual, &[rank("twoturnmove"), rank("stall"), rank("phantomforce"), rank("flinch")], Hk::Pokemon));
        assert!(!wl.contains(Ev::Residual, &[rank("stall")], Hk::Pokemon));
    }

    // Minimal SHA-256 for the test (no extra dependency).
    fn sha256_hex(data: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
            0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
            0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
            0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
            0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];
        let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
        let mut msg = data.to_vec();
        let bits = (data.len() as u64) * 8;
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bits.to_be_bytes());
        for chunk in msg.chunks(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            }
            let mut v = h;
            for i in 0..64 {
                let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
                let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
                let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
                let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
                let t2 = s0.wrapping_add(maj);
                v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
            }
            for i in 0..8 {
                h[i] = h[i].wrapping_add(v[i]);
            }
        }
        h.iter().map(|x| format!("{x:08x}")).collect()
    }
}
