//! vgczero engine: a fast, copyable Gen 9 Champions VGC doubles simulator
//! for self-play reinforcement learning and search.
//!
//! Data (dex, teams) is exported from Pokemon Showdown, which remains the
//! source of truth; `tests/` and `../showdown/parity` check the engine against it.

pub mod actions;
pub mod baseline;
pub mod battle;
pub mod dex;
pub mod fixed;
pub mod kinds;
pub mod obs;
pub mod prof;
pub mod rng;
pub mod observed;
pub mod snapshot;
pub mod state;
pub mod teams;
pub mod types;
pub mod vecenv;
pub mod worlds;

#[cfg(feature = "python")]
mod py;

pub use actions::{Choice, SlotAction, N_PREVIEW_ACTIONS, N_SLOT_ACTIONS};
pub use battle::{Battle, BattleConfig, Phase, Request, RequestKind};
pub use dex::{dex, load_dex};
pub use teams::{load_teams, TeamSpec};

/// Generated inputs (dex, team pool, parity fixtures) are rebuilt from the pinned Showdown by
/// `adapters/vgczero` and kept outside Git. `FAST_ENGINE_DATA` overrides the default directory:
/// the Quark Drive checkout's cache, or `../data` of a standalone vgczero tree.
pub fn data_path(name: &str) -> String {
    let dir = std::env::var("FAST_ENGINE_DATA").unwrap_or_else(|_| {
        let repo = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../.cache/model_train/fast_engine/vgczero");
        if std::path::Path::new(repo).is_dir() {
            repo.to_string()
        } else {
            concat!(env!("CARGO_MANIFEST_DIR"), "/../data").to_string()
        }
    });
    format!("{dir}/{name}")
}
