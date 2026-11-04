//! Native observation tracker: the Rust port of `model_train/env/tracker.cjs`.
//!
//! It consumes one player's protocol channel (their own `|split|` secrets) plus that player's own
//! request and keeps what that player may know. Validated against the JS tracker by feeding the
//! same Showdown channel lines to both (`adapters/vgczero/parity/track.js` produces the fixture);
//! the native state must equal the JS `canonTracker` state, and `obs`/`masks` the JS tensors.
//!
//! State is kept in the same shapes the JS tracker uses so the two can be compared directly; ids
//! are kept as `String` (toID-normalized for items/abilities/moves/nature, display names for
//! species and types). This module is not on the simulation hot path.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::dex::dex;
use crate::omap::Omap;

pub const BOOSTS: [&str; 7] = ["atk", "def", "spa", "spd", "spe", "accuracy", "evasion"];

// Informational lines with no state of their own (results are carried by -damage/-heal/etc.).
const IGNORED: &[&str] = &[
    "", "t:", "j", "J", "l", "L", "c", "n", "N", "raw", "html", "uhtml", "uhtmlchange", "inactive",
    "inactiveoff", "badge", "rated", "gen", "tier", "rule", "gametype", "title", "seed", "clearpoke",
    "teampreview", "start", "debug", "message", "-message", "-hint", "-anim", "-crit", "-supereffective",
    "-resisted", "-immune", "-miss", "-block", "-notarget", "-ohko", "-hitcount", "-nothing", "-zbroken",
    "-combine", "-waiting", "timestamp", "bigerror", "error",
];

/// Moves the engine re-queues as the user's own action (a Round partner, the second Pledge,
/// Pursuit on a switch-out): their `[from]` does not make the action "derived".
const REQUEUED: &[&str] = &["move: Round", "move: Fire Pledge", "move: Grass Pledge", "move: Water Pledge", "move: Pursuit"];
/// Protection the engine keeps on the side though the protocol announces it on a Pokemon.
const SIDE_SINGLETURN: &[&str] = &["wideguard", "quickguard", "craftyshield", "matblock"];

/// Showdown's `toID`: lowercase, keep `[a-z0-9]`.
pub fn to_id(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// `move: Tailwind` / `ability: Flash Fire` / `Reflect` -> showdown id.
pub fn effect_id(text: &str) -> String {
    let t = text.strip_prefix("move:").or_else(|| text.strip_prefix("ability:")).or_else(|| text.strip_prefix("item:")).unwrap_or(text);
    to_id(t.trim())
}

/// A parsed `p1a: Name` ident.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentFull {
    pub side: u8,
    pub slot: Option<u8>,
    pub name: String,
}

/// `p1a: Name` -> (side index, slot, name). Returns None for an unparsed ident.
pub fn parse_ident(text: &str) -> Option<IdentFull> {
    let bytes = text.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'p' || (bytes[1] != b'1' && bytes[1] != b'2') {
        return None;
    }
    let side = (bytes[1] - b'1') as u8;
    let mut i = 2;
    let mut slot = None;
    if bytes.len() > i && (bytes[i] == b'a' || bytes[i] == b'b') {
        slot = Some(bytes[i] - b'a');
        i += 1;
    }
    // Expect `:` then optional space.
    if bytes.len() <= i || bytes[i] != b':' {
        return None;
    }
    i += 1;
    let rest = &text[i..];
    let name = rest.strip_prefix(' ').unwrap_or(rest).to_string();
    Some(IdentFull { side, slot, name })
}

#[derive(Clone, Debug)]
pub struct Hp {
    pub fraction: f64,
    pub exact: Option<[f64; 2]>,
    pub status: String,
    pub fainted: bool,
}

/// `55/224`, `72/100 par`, `0 fnt`, Champions `20/100y`.
pub fn parse_hp(text: &str) -> Option<Hp> {
    let mut it = text.split(' ');
    let hp = it.next().unwrap_or("");
    let status = it.next().unwrap_or("").to_string();
    if hp == "0" || status == "fnt" {
        return Some(Hp { fraction: 0.0, exact: None, status: String::new(), fainted: true });
    }
    // ^(\d+)/(\d+)([gyr])?$
    let (num, rest) = hp.split_once('/')?;
    let now: f64 = num.parse().ok()?;
    let (max_str, flag) = match rest.chars().last() {
        Some(c @ ('g' | 'y' | 'r')) => (&rest[..rest.len() - 1], Some(c)),
        _ => (rest, None),
    };
    let max: f64 = max_str.parse().ok()?;
    // Champions shows floor(percent) to others, plus whether HP is above the 20/50 thresholds.
    let above = if flag == Some('y') && now == 20.0 || flag == Some('g') && now == 50.0 { 0.005 } else { 0.0 };
    Some(Hp { fraction: now / max + above, exact: Some([now, max]), status, fainted: false })
}

/// Species by a details/name string (first comma segment), toID-normalized lookup.
fn species_of(details: &str) -> Option<&'static crate::dex::SpeciesData> {
    let first = details.split(',').next().unwrap_or("");
    let id = crate::dex::dex().species_id(&to_id(first))?;
    Some(crate::dex::dex().sp(id))
}

fn base_species_of_name(name: &str) -> Option<String> {
    species_of(name).map(|s| s.base_species.clone())
}

#[derive(Clone, Debug)]
pub struct Mon {
    pub index: i32,
    pub name: Option<String>,
    pub species: Option<String>,
    pub forme: Option<String>,
    pub item: Option<String>,
    pub ability: Option<String>,
    pub base_ability: Option<String>,
    pub moves: Vec<String>,
    pub nature: Option<String>,
    pub gender: String,
    pub hp: f64,
    pub exact: Option<[f64; 2]>,
    pub status: String,
    pub fainted: bool,
    pub seen: bool,
    pub brought: Option<bool>,
    pub slot: Option<i32>,
    pub sleep_elapsed: f64,
    pub sleep_rest: bool,
    pub entered_turn: f64,
    pub moves_since_entry: f64,
    pub stall: Option<Value>,
    pub tox_stage: f64,
    pub last_move_turn: Option<f64>,
    pub boosts: BTreeMap<String, i32>,
    pub volatiles: Omap<Value>,
    pub types: Option<Vec<String>>,
    pub me_evolved: bool,
    pub item_gone: bool,
    pub stats: Option<Value>,
    pub last_move: Option<String>,
    pub snapshot: Option<Value>,
    pub commanding: Option<bool>,
    pub identity_confirmed: Option<bool>,
    pub confused_with: Option<i32>,
    /// Transient revival-target marker; not part of the canonical state.
    pub revived: bool,
}

impl Mon {
    fn new(index: i32, species: Option<&str>, item: Option<&str>, ability: Option<&str>, moves: &[String], nature: Option<&str>, gender: &str) -> Mon {
        let sp = species.and_then(species_of);
        let name = sp.map(|s| s.name.clone());
        Mon {
            index,
            name: None,
            species: name.clone(),
            forme: name,
            item: item.map(to_id),
            ability: ability.map(to_id),
            base_ability: ability.map(to_id),
            moves: moves.to_vec(),
            nature: nature.map(to_id),
            gender: gender.to_string(),
            hp: 1.0,
            exact: None,
            status: String::new(),
            fainted: false,
            seen: false,
            brought: None,
            slot: None,
            sleep_elapsed: 0.0,
            sleep_rest: false,
            entered_turn: 0.0,
            moves_since_entry: 0.0,
            stall: None,
            tox_stage: 0.0,
            last_move_turn: None,
            boosts: BOOSTS.iter().map(|b| (b.to_string(), 0)).collect(),
            volatiles: Omap::new(),
            types: None,
            me_evolved: false,
            item_gone: false,
            stats: None,
            last_move: None,
            snapshot: None,
            commanding: None,
            identity_confirmed: None,
            confused_with: None,
            revived: false,
        }
    }

    fn to_value(&self) -> Value {
        let boosts = Value::Object(self.boosts.iter().map(|(k, v)| (k.clone(), json!(v))).collect());
        let volatiles = Value::Object(self.volatiles.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        let mut m = Map::new();
        m.insert("index".into(), json!(self.index));
        m.insert("name".into(), opt(&self.name));
        m.insert("species".into(), opt(&self.species));
        m.insert("forme".into(), opt(&self.forme));
        m.insert("item".into(), opt(&self.item));
        m.insert("ability".into(), opt(&self.ability));
        m.insert("baseAbility".into(), opt(&self.base_ability));
        m.insert("moves".into(), json!(self.moves));
        m.insert("nature".into(), opt(&self.nature));
        m.insert("gender".into(), json!(self.gender));
        m.insert("hp".into(), json!(self.hp));
        m.insert("exact".into(), match self.exact { Some([a, b]) => json!([a, b]), None => Value::Null });
        m.insert("status".into(), json!(self.status));
        m.insert("fainted".into(), json!(self.fainted));
        m.insert("seen".into(), json!(self.seen));
        m.insert("brought".into(), self.brought.map(|b| json!(b)).unwrap_or(Value::Null));
        m.insert("slot".into(), self.slot.map(|s| json!(s)).unwrap_or(Value::Null));
        m.insert("sleepElapsed".into(), json!(self.sleep_elapsed));
        m.insert("sleepRest".into(), json!(self.sleep_rest));
        m.insert("enteredTurn".into(), json!(self.entered_turn));
        m.insert("movesSinceEntry".into(), json!(self.moves_since_entry));
        m.insert("stall".into(), self.stall.clone().unwrap_or(Value::Null));
        m.insert("toxStage".into(), json!(self.tox_stage));
        m.insert("lastMoveTurn".into(), self.last_move_turn.map(|t| json!(t)).unwrap_or(Value::Null));
        m.insert("boosts".into(), boosts);
        m.insert("volatiles".into(), volatiles);
        m.insert("types".into(), self.types.as_ref().map(|t| json!(t)).unwrap_or(Value::Null));
        m.insert("megaEvolved".into(), json!(self.me_evolved));
        m.insert("itemGone".into(), json!(self.item_gone));
        m.insert("stats".into(), self.stats.clone().unwrap_or(Value::Null));
        m.insert("lastMove".into(), opt(&self.last_move));
        m.insert("snapshot".into(), self.snapshot.clone().unwrap_or(Value::Null));
        m.insert("commanding".into(), self.commanding.map(|b| json!(b)).unwrap_or(Value::Null));
        m.insert("identityConfirmed".into(), self.identity_confirmed.map(|b| json!(b)).unwrap_or(Value::Null));
        m.insert("confusedWith".into(), self.confused_with.map(|i| json!(i)).unwrap_or(Value::Null));
        Value::Object(m)
    }
}

fn opt(v: &Option<String>) -> Value {
    v.as_ref().map(|s| json!(s)).unwrap_or(Value::Null)
}

#[derive(Clone, Debug)]
pub struct Side {
    pub id: String,
    pub player: Option<String>,
    pub team_size: Option<f64>,
    pub mega_used: bool,
    pub sheet: Option<bool>,
    pub active: [Option<i32>; 2],
    pub conditions: Omap<Value>,
    pub names: BTreeMap<String, i32>,
    pub team: Vec<Mon>,
}

impl Side {
    fn new(id: &str) -> Side {
        Side {
            id: id.into(),
            player: None,
            team_size: None,
            mega_used: false,
            sheet: None,
            active: [None, None],
            conditions: Omap::new(),
            names: BTreeMap::new(),
            team: Vec::new(),
        }
    }

    fn to_value(&self) -> Value {
        let conditions = Value::Object(self.conditions.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
        let names = Value::Object(self.names.iter().map(|(k, v)| (k.clone(), json!(v))).collect());
        let mut m = Map::new();
        m.insert("id".into(), json!(self.id));
        m.insert("player".into(), self.player.as_ref().map(|s| json!(s)).unwrap_or(Value::Null));
        m.insert("teamSize".into(), self.team_size.map(|n| json!(n)).unwrap_or(Value::Null));
        m.insert("megaUsed".into(), json!(self.mega_used));
        m.insert("sheet".into(), self.sheet.map(|b| json!(b)).unwrap_or(Value::Null));
        m.insert("active".into(), json!([self.active[0], self.active[1]]));
        m.insert("conditions".into(), conditions);
        m.insert("names".into(), names);
        m.insert("team".into(), Value::Array(self.team.iter().map(|m| m.to_value()).collect()));
        Value::Object(m)
    }
}

#[derive(Clone, Debug)]
pub struct Action {
    pub(crate) side: String,
    pub(crate) slot: Option<i32>,
    pub(crate) actor: i32,
    pub(crate) kind: String,
    pub(crate) id: Option<String>,
    pub(crate) target: Option<(String, i32)>,
    pub(crate) spread: bool,
    pub(crate) missed: bool,
    pub(crate) order: i64,
    pub(crate) phase: String,
}

impl Action {
    fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("side".into(), json!(self.side));
        m.insert("slot".into(), self.slot.map(|s| json!(s)).unwrap_or(Value::Null));
        m.insert("actor".into(), json!(self.actor));
        m.insert("kind".into(), json!(self.kind));
        m.insert("id".into(), self.id.as_ref().map(|s| json!(s)).unwrap_or(Value::Null));
        m.insert("target".into(), match &self.target {
            Some((s, sl)) => json!({"side": s, "slot": sl}),
            None => Value::Null,
        });
        m.insert("spread".into(), json!(self.spread));
        m.insert("missed".into(), json!(self.missed));
        m.insert("order".into(), json!(self.order));
        m.insert("phase".into(), json!(self.phase));
        Value::Object(m)
    }
}

#[derive(Clone, Debug)]
pub struct Hist {
    pub(crate) turn: i64,
    pub(crate) actions: Vec<Action>,
}

/// Weather/terrain/pseudo-weather the tracker holds (encoded by `encoder.rs`).
#[derive(Clone, Debug, Default)]
pub struct Field {
    pub(crate) weather: Option<Value>,
    pub(crate) terrain: Option<Value>,
    pub(crate) pseudo: Omap<Value>,
}

pub struct Tracker {
    pub me: String,
    pub foe: String,
    pub spectator: bool,
    pub phase: String,
    pub turn: i64,
    pub order: i64,
    pub(crate) sides: [Side; 2],
    pub(crate) field: Field,
    pub(crate) history: Vec<Hist>,
    pub(crate) request: Option<Value>,
    pub ended: bool,
    pub winner: Option<String>,
    last_move_by_slot: BTreeMap<String, String>,
    last_mover: Option<i32>,
    hidden_lock_until: Option<i64>,
}

fn side_key(side: u8) -> &'static str {
    if side == 0 { "p1" } else { "p2" }
}

impl Tracker {
    pub fn new(perspective: u8, spectator: bool) -> Tracker {
        Tracker {
            me: side_key(perspective).into(),
            foe: side_key(1 - perspective).into(),
            spectator,
            phase: "turn".into(),
            turn: 0,
            order: 0,
            sides: [Side::new("p1"), Side::new("p2")],
            field: Field::default(),
            history: Vec::new(),
            request: None,
            ended: false,
            winner: None,
            last_move_by_slot: BTreeMap::new(),
            last_mover: None,
            hidden_lock_until: None,
        }
    }

    pub fn feed(&mut self, lines: &[String]) {
        for line in lines {
            self.line(line);
        }
    }

    fn line(&mut self, line: &str) {
        if !line.starts_with('|') {
            return;
        }
        let parts: Vec<&str> = line.split('|').collect();
        let type_ = parts.get(1).copied().unwrap_or("");
        let args: Vec<String> = parts.iter().skip(2).map(|s| s.to_string()).collect();
        let mut tags: BTreeMap<String, String> = BTreeMap::new();
        for p in &args {
            if let Some(rest) = p.strip_prefix('[') {
                if let Some(end) = rest.find(']') {
                    let key = &rest[..end];
                    if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric()) {
                        let value = rest[end + 1..].trim_start().to_string();
                        tags.insert(key.to_string(), value);
                    }
                }
            }
        }
        let a = &args;
        match type_ {
            "player" => self.h_player(a),
            "teamsize" => self.h_teamsize(a),
            "poke" => self.h_poke(a),
            "showteam" => self.h_showteam(a),
            "upkeep" => self.h_upkeep(),
            "turn" => self.h_turn(a),
            "switch" => self.h_switch(a, &tags),
            "drag" => self.h_drag(a),
            "replace" => self.h_replace(a),
            "detailschange" => self.h_detailschange(a),
            "-formechange" => self.h_formechange(a),
            "-mega" => self.h_mega(a),
            "move" => self.h_move(a, &tags),
            "cant" => self.h_cant(a, &tags),
            "faint" => self.h_faint(a),
            "-damage" => self.h_damage(a, &tags),
            "-fail" => self.h_fail(a),
            "-heal" => self.h_heal(a, &tags),
            "-sethp" => self.h_sethp(a),
            "-status" => self.h_status(a, &tags),
            "-curestatus" => self.h_curestatus(a),
            "-cureteam" => self.h_cureteam(a),
            "-boost" => self.h_boost(a, 1),
            "-unboost" => self.h_boost(a, -1),
            "-setboost" => self.h_setboost(a),
            "-clearboost" => self.h_clearboost(a),
            "-clearallboost" => self.h_clearallboost(),
            "-clearnegativeboost" => self.h_clear_neg(a),
            "-clearpositiveboost" => self.h_clear_pos(a),
            "-invertboost" => self.h_invertboost(a),
            "-copyboost" => self.h_copyboost(a),
            "-swapboost" => self.h_swapboost(a),
            "-weather" => self.h_weather(a, &tags),
            "-fieldstart" => self.h_fieldstart(a),
            "-fieldactivate" => self.h_fieldactivate(a),
            "-fieldend" => self.h_fieldend(a),
            "-sidestart" => self.h_sidestart(a),
            "-sideend" => self.h_sideend(a),
            "-swapsideconditions" => self.h_swapsideconditions(),
            "-start" => self.h_start(a, &tags),
            "-end" => self.h_end(a),
            "-singleturn" => self.h_singleturn(a),
            "-singlemove" => self.h_singlemove(a),
            "-mustrecharge" => self.h_mustrecharge(a),
            "-prepare" => self.h_prepare(a),
            "-enditem" => self.h_enditem(a),
            "-item" => self.h_item(a, &tags),
            "-ability" => self.h_ability(a, &tags),
            "-activate" => self.h_activate(a, &tags),
            "-endability" => self.h_endability(a),
            "-transform" => self.h_transform(a),
            "swap" => self.h_swap(a),
            "win" => self.h_win(a),
            "tie" => { self.ended = true; self.winner = None; }
            _ => {
                if !IGNORED.contains(&type_) {
                    panic!("Unknown protocol line: {line}");
                }
            }
        }
    }

    // ---- identification -------------------------------------------------------------------
    fn mon(&mut self, ident: &str, details: Option<&str>) -> usize {
        let id = parse_ident(ident).unwrap_or_else(|| panic!("Unparsed ident: {ident}"));
        let side = &mut self.sides[id.side as usize];
        if id.slot.is_some() && details.is_none() {
            let slot = id.slot.unwrap() as usize;
            if let Some(index) = side.active[slot] {
                return index as usize;
            }
            let named = side.names.get(&id.name).copied();
            match named {
                Some(i) if side.team[i as usize].slot.is_none() => {
                    side.active[slot] = Some(i);
                    side.team[i as usize].slot = Some(slot as i32);
                    return i as usize;
                }
                _ => panic!("No Pokemon tracked at {ident}"),
            }
        }
        if details.is_none() {
            if let Some(i) = side.names.get(&id.name) {
                return *i as usize;
            }
        }
        let sp = details.and_then(species_of);
        let base = sp.map(|s| s.base_species.clone());
        let mut found = side.names.get(&id.name).copied();
        if found.is_none() {
            if let Some(base) = &base {
                found = side
                    .team
                    .iter()
                    .position(|m| m.species.as_ref().and_then(|s| base_species_of_name(s)).as_deref() == Some(base.as_str()))
                    .map(|i| i as i32);
            }
        }
        let index = match found {
            Some(i) => i,
            None => {
                if side.team.len() >= 6 {
                    panic!("Cannot place {ident} ({details:?}) in p{}'s team", id.side + 1);
                }
                let mut mon = Mon::new(side.team.len() as i32, None, None, None, &[], None, "");
                if let Some(sp) = sp {
                    mon.species = Some(sp.name.clone());
                    mon.forme = Some(sp.name.clone());
                }
                side.team.push(mon);
                (side.team.len() - 1) as i32
            }
        };
        let side = &mut self.sides[id.side as usize];
        let cur_name = side.team[index as usize].name.clone();
        if cur_name.as_deref() != Some(id.name.as_str()) {
            if let Some(old) = cur_name {
                side.names.remove(&old);
            }
            side.team[index as usize].name = Some(id.name.clone());
            side.names.insert(id.name.clone(), index);
        }
        index as usize
    }

    /// Showdown's `sideOf`: just the leading `p1`/`p2` (bare strings like `p1`, or `p1: Name`).
    fn side_index(text: &str) -> usize {
        if text.starts_with("p1") { 0 } else if text.starts_with("p2") { 1 } else { panic!("Unparsed side: {text}") }
    }

    // ---- input ----------------------------------------------------------------------------
    fn record(&mut self, mut action: Action) {
        if let Some(current) = self.history.last() {
            if current.turn != self.turn {
                return;
            }
        } else {
            return;
        }
        action.order = self.order;
        self.order += 1;
        action.phase = self.phase.clone();
        self.history.last_mut().unwrap().actions.push(action);
    }

    // ---- canonical state ------------------------------------------------------------------
    /// The player own request last observed (read-only).
    pub fn request(&self) -> Option<&serde_json::Value> {
        self.request.as_ref()
    }

    pub fn history(&self) -> &[Hist] {
        &self.history
    }

    pub fn to_value(&self) -> Value {
        let field = {
            let pseudo = Value::Object(self.field.pseudo.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
            json!({"weather": self.field.weather.clone().unwrap_or(Value::Null),
                   "terrain": self.field.terrain.clone().unwrap_or(Value::Null),
                   "pseudo": pseudo})
        };
        let history = Value::Array(self.history.iter().map(|h| {
            json!({"turn": h.turn, "actions": h.actions.iter().map(|a| a.to_value()).collect::<Vec<_>>()})
        }).collect());
        let lmb = Value::Object(self.last_move_by_slot.iter().map(|(k, v)| (k.clone(), json!(v))).collect());
        json!({
            "me": self.me, "foe": self.foe, "spectator": self.spectator, "phase": self.phase,
            "turn": self.turn, "order": self.order, "ended": self.ended, "winner": self.winner,
            "hiddenLockUntil": self.hidden_lock_until, "lastMover": self.last_mover,
            "field": field,
            "sides": {"p1": self.sides[0].to_value(), "p2": self.sides[1].to_value()},
            "history": history,
            "lastMoveBySlot": lmb,
        })
    }

    // ---- record helpers -------------------------------------------------------------------
    fn side_of(&self, text: &str) -> usize {
        Self::side_index(text)
    }

    fn me_side(&self) -> usize {
        if self.me == "p1" { 0 } else { 1 }
    }

    pub(crate) fn turns_since_entry(&self, m: &Mon) -> f64 {
        self.turn as f64 - m.entered_turn
    }

    pub(crate) fn stall_streak(&self, m: &Mon) -> i64 {
        let Some(s) = &m.stall else { return 0 };
        let t = s.get("turn").and_then(|v| v.as_i64()).unwrap_or(-1);
        let count = s.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
        if t == self.turn || (t == self.turn - 1 && self.phase != "upkeep") { count } else { 0 }
    }

    fn apply_hp(&mut self, side: usize, idx: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        let hp = parse_hp(text).unwrap_or_else(|| panic!("Unparsed HP: {text}"));
        let is_me = !self.spectator && side == self.me_side();
        let m = &mut self.sides[side].team[idx];
        m.hp = hp.fraction;
        m.status = if hp.fainted { String::new() } else { hp.status.clone() };
        m.fainted = hp.fainted;
        m.exact = if is_me { hp.exact } else { None };
    }

    fn reassign_actions(&mut self, side: usize, slot: Option<i32>, map: &BTreeMap<i32, i32>) {
        let side_id = side_key(side as u8);
        for h in &mut self.history {
            for a in &mut h.actions {
                if a.side == side_id && (slot.is_none() || a.slot == slot) {
                    if let Some(n) = map.get(&a.actor) {
                        a.actor = *n;
                    }
                }
            }
        }
    }

    fn swap_observations(&mut self, a: usize, b: usize, side: usize) {
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        {
            let (l, r) = self.sides[side].team.split_at_mut(hi);
            let (x, y) = (&mut l[lo], &mut r[0]);
            // x is the lower-index mon; y the higher.
            let (p, q) = if a < b { (x, y) } else { (y, x) };
            std::mem::swap(&mut p.hp, &mut q.hp);
            std::mem::swap(&mut p.exact, &mut q.exact);
            std::mem::swap(&mut p.status, &mut q.status);
            std::mem::swap(&mut p.fainted, &mut q.fainted);
            std::mem::swap(&mut p.sleep_elapsed, &mut q.sleep_elapsed);
            std::mem::swap(&mut p.sleep_rest, &mut q.sleep_rest);
            std::mem::swap(&mut p.tox_stage, &mut q.tox_stage);
            std::mem::swap(&mut p.last_move, &mut q.last_move);
            std::mem::swap(&mut p.last_move_turn, &mut q.last_move_turn);
        }
        for i in [a, b] {
            if !self.sides[side].team[i].me_evolved {
                self.sides[side].team[i].forme = self.sides[side].team[i].species.clone();
            }
        }
        let map = BTreeMap::from([(a as i32, b as i32), (b as i32, a as i32)]);
        self.reassign_actions(side, None, &map);
    }

    fn volatile(text: &str, extra: &[(&str, Value)]) -> (String, Value) {
        let raw = effect_id(text);
        let counted = parse_counted(&raw);
        let id = counted.as_ref().map(|(n, _)| n.clone()).unwrap_or_else(|| raw.clone());
        let mut value = Map::new();
        for (k, v) in extra {
            value.insert((*k).to_string(), v.clone());
        }
        if !extra.iter().any(|(k, _)| *k == "since") {
            value.insert("since".into(), Value::Null);
        }
        value.insert("count".into(), match &counted { Some((_, n)) => json!(n), None => Value::Null });
        (id, Value::Object(value))
    }

    fn snapshot_of(m: &Mon) -> Value {
        json!({"seen": m.seen, "hp": m.hp, "exact": m.exact, "status": m.status, "fainted": m.fainted,
               "item": m.item, "itemGone": m.item_gone, "ability": m.ability, "sleepElapsed": m.sleep_elapsed,
               "sleepRest": m.sleep_rest, "lastMove": m.last_move, "lastMoveTurn": m.last_move_turn})
    }

    fn mon_side(&self, text: &str) -> usize {
        parse_ident(text).unwrap().side as usize
    }

    // ---- handlers -------------------------------------------------------------------------
    fn h_player(&mut self, a: &[String]) {
        let side = self.side_of(&a[0]);
        self.sides[side].player = a.get(1).cloned();
    }
    fn h_teamsize(&mut self, a: &[String]) {
        let side = self.side_of(&a[0]);
        self.sides[side].team_size = a.get(1).and_then(|s| s.parse().ok());
    }
    fn h_poke(&mut self, a: &[String]) {
        let side = self.side_of(&a[0]);
        let Some(sp) = species_of(a.get(1).map(|s| s.as_str()).unwrap_or("")) else { return };
        let base = sp.base_species.clone();
        let name = sp.name.clone();
        let has = self.sides[side].team.iter().any(|m| m.species.as_ref().and_then(|s| base_species_of_name(s)).as_deref() == Some(base.as_str()));
        if !has {
            let mut mon = Mon::new(self.sides[side].team.len() as i32, None, None, None, &[], None, "");
            mon.species = Some(name.clone());
            mon.forme = Some(name);
            self.sides[side].team.push(mon);
        }
    }
    fn h_showteam(&mut self, a: &[String]) {
        let side = self.side_of(&a[0]);
        let packed = a[1..].join("|");
        for (i, set) in unpack_sets(&packed).into_iter().enumerate() {
            let species_field = set.get("species").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let name_field = set.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let sp_name = if species_field.is_empty() { name_field } else { species_field };
            let Some(sp) = species_of(&sp_name) else { continue };
            let base = sp.base_species.clone();
            let name = sp.name.clone();
            let existing = self.sides[side].team.iter().position(|m| m.species.as_ref().and_then(|s| base_species_of_name(s)).as_deref() == Some(base.as_str()));
            let moves: Vec<String> = set.get("moves").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(to_id).collect()).unwrap_or_default();
            let item = set.get("item").and_then(|v| v.as_str()).unwrap_or("");
            let ability = set.get("ability").and_then(|v| v.as_str()).unwrap_or("");
            let nature = set.get("nature").and_then(|v| v.as_str()).unwrap_or("");
            let gender = set.get("gender").and_then(|v| v.as_str()).unwrap_or("");
            let idx = existing.unwrap_or(i);
            let fresh = Mon::new(idx as i32, Some(&name), Some(item), Some(ability), &moves, Some(nature), gender);
            if existing.is_some() {
                let old = &mut self.sides[side].team[idx];
                old.item = fresh.item;
                old.ability = fresh.ability;
                old.base_ability = fresh.base_ability;
                old.moves = fresh.moves;
                old.nature = fresh.nature;
                old.gender = fresh.gender;
                if old.species.is_none() { old.species = fresh.species.clone(); }
                if old.forme.is_none() { old.forme = fresh.forme.clone(); }
            } else {
                while self.sides[side].team.len() <= idx {
                    self.sides[side].team.push(Mon::new(0, None, None, None, &[], None, ""));
                }
                for (k, m) in self.sides[side].team.iter_mut().enumerate() { m.index = k as i32; }
                self.sides[side].team[idx] = fresh;
            }
        }
        self.sides[side].sheet = Some(true);
    }
    fn h_upkeep(&mut self) {
        self.phase = "upkeep".into();
        let turn = self.turn;
        self.field.pseudo.retain(|_, v| v.get("endsAtUpkeep").and_then(|x| x.as_i64()).map(|e| e > turn).unwrap_or(true));
        for s in 0..2 {
            self.sides[s].conditions.retain(|_, v| v.get("singleturn").and_then(|x| x.as_bool()) != Some(true));
        }
    }
    fn h_turn(&mut self, a: &[String]) {
        self.turn = a.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        self.order = 0;
        self.phase = "turn".into();
        self.history.push(Hist { turn: self.turn, actions: Vec::new() });
        if self.history.len() > 3 {
            self.history.remove(0);
        }
        for s in 0..2 {
            self.sides[s].conditions.retain(|_, v| v.get("singleturn").and_then(|x| x.as_bool()) != Some(true));
            for m in &mut self.sides[s].team {
                m.volatiles.retain(|_, v| v.get("singleturn").and_then(|x| x.as_bool()) != Some(true));
            }
        }
    }
    fn h_switch(&mut self, a: &[String], tags: &BTreeMap<String, String>) {
        let id = parse_ident(&a[0]).unwrap();
        let details = a.get(1).map(|s| s.as_str());
        let hp = a.get(2).map(|s| s.as_str());
        let mon = self.incoming(&a[0], details, hp);
        self.set_active(id.side as usize, id.slot.unwrap() as usize, mon, details.unwrap_or(""), hp.unwrap_or(""), tags.get("from").map(|s| s.as_str()));
        let actor = self.sides[id.side as usize].team[mon].index;
        self.record(Action { side: side_key(id.side).into(), slot: Some(id.slot.unwrap() as i32), actor, kind: "switch".into(), id: None, target: None, spread: false, missed: false, order: 0, phase: String::new() });
    }
    fn h_drag(&mut self, a: &[String]) {
        let id = parse_ident(&a[0]).unwrap();
        let details = a.get(1).map(|s| s.as_str());
        let hp = a.get(2).map(|s| s.as_str());
        let mon = self.incoming(&a[0], details, hp);
        self.set_active(id.side as usize, id.slot.unwrap() as usize, mon, details.unwrap_or(""), hp.unwrap_or(""), Some("drag"));
    }
    fn h_replace(&mut self, a: &[String]) {
        let real = self.mon(&a[0], a.get(1).map(|s| s.as_str()));
        self.reveal(&a[0], real);
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        if let Some(sp) = species_of(a.get(1).map(|s| s.as_str()).unwrap_or("")) {
            self.sides[side].team[idx].forme = Some(sp.name.clone());
        }
        self.sides[side].team[idx].identity_confirmed = Some(true);
    }
    fn h_detailschange(&mut self, a: &[String]) {
        let mut idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        let Some(sp) = species_of(a.get(1).map(|s| s.as_str()).unwrap_or("")) else { return };
        let id = parse_ident(&a[0]).unwrap();
        let named = self.sides[side].names.get(&id.name).copied().map(|n| n as usize);
        let cur = self.sides[side].team[idx].species.clone();
        let mismatch = cur.as_ref().and_then(|s| base_species_of_name(s)).as_deref() != Some(sp.base_species.as_str());
        if mismatch {
            if let Some(n) = named {
                if n != idx {
                    self.reveal(&a[0], n);
                    idx = self.mon(&a[0], None);
                }
            }
        }
        self.sides[side].team[idx].types = None;
        self.sides[side].team[idx].forme = Some(sp.name.clone());
        if sp.is_mega {
            self.sides[side].team[idx].me_evolved = true;
            if let Some(ab) = sp.abilities.first() {
                self.sides[side].team[idx].ability = Some(to_id(ab));
            }
        }
    }
    fn h_formechange(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        if let Some(sp) = species_of(a.get(1).map(|s| s.as_str()).unwrap_or("")) {
            self.sides[side].team[idx].forme = Some(sp.name.clone());
        }
        self.sides[side].team[idx].types = None;
    }
    fn h_mega(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let id = parse_ident(&a[0]).unwrap();
        let side = id.side as usize;
        self.sides[side].mega_used = true;
        self.sides[side].team[idx].me_evolved = true;
        if let Some(it) = a.get(2) {
            self.sides[side].team[idx].item = Some(to_id(it));
        }
    }
    fn h_move(&mut self, a: &[String], tags: &BTreeMap<String, String>) {
        let id = parse_ident(&a[0]).unwrap();
        let move_id = to_id(a.get(1).map(|s| s.as_str()).unwrap_or(""));
        if let Some(mid) = dex().move_id(&move_id) {
            if dex().mv(mid).self_volatile == Some(crate::dex::VolKind::LockedMove) {
                self.hidden_lock_until = Some(self.turn + 3);
            }
        }
        let from = tags.get("from").cloned();
        let derived = from.as_deref().map(|f| !f.contains("lockedmove") && !REQUEUED.contains(&f)).unwrap_or(false);
        if !derived {
            let f = Fits::Move(move_id.clone());
            self.explain(&a[0], &f, move_id != "struggle" && move_id != "recharge");
        }
        let idx = self.mon(&a[0], None);
        let side = id.side as usize;
        let keys: Vec<String> = self.sides[side].team[idx].volatiles.iter()
            .filter(|(v, val)| val.get("singlemove").and_then(|x| x.as_bool()) == Some(true) || *v == "twoturnmove")
            .map(|(k, _)| k.clone()).collect();
        for k in keys { self.sides[side].team[idx].volatiles.remove(&k); }
        if from.as_deref() == Some("ability: Dancer") {
            self.sides[side].team[idx].moves_since_entry += 1.0;
        }
        if !derived {
            self.sides[side].team[idx].last_move = Some(move_id.clone());
            self.sides[side].team[idx].last_move_turn = Some(self.turn as f64);
            self.sides[side].team[idx].moves_since_entry += 1.0;
            self.last_mover = Some(self.sides[side].team[idx].index);
            if stall_adds(&move_id) {
                let streak = self.stall_streak(&self.sides[side].team[idx]);
                let before = self.sides[side].team[idx].stall.as_ref()
                    .map(|s| json!({"turn": s.get("turn").cloned().unwrap_or(Value::Null), "count": s.get("count").cloned().unwrap_or(Value::Null)}))
                    .unwrap_or(Value::Null);
                self.sides[side].team[idx].stall = Some(json!({"turn": self.turn, "count": streak + 1, "before": before}));
            }
            let key = format!("{}{}", side_key(id.side), id.slot.map(|s| s.to_string()).unwrap_or_else(|| "null".into()));
            self.last_move_by_slot.insert(key, move_id.clone());
            let target = a.get(2).and_then(|t| parse_ident(t)).and_then(|t| t.slot.map(|sl| (side_key(t.side).to_string(), sl as i32)));
            let actor = self.sides[side].team[idx].index;
            self.record(Action { side: side_key(id.side).into(), slot: id.slot.map(|s| s as i32), actor, kind: "move".into(), id: Some(move_id), target, spread: tags.contains_key("spread"), missed: tags.contains_key("miss") || tags.contains_key("notarget"), order: 0, phase: String::new() });
        }
    }
    fn h_cant(&mut self, a: &[String], tags: &BTreeMap<String, String>) {
        if tags.contains_key("of") {
            return;
        }
        let id = parse_ident(&a[0]).unwrap();
        let idx = self.mon(&a[0], None);
        let side = id.side as usize;
        let eff = effect_id(a.get(1).map(|s| s.as_str()).unwrap_or(""));
        if eff == "slp" {
            let inc = if self.sides[side].team[idx].ability.as_deref() == Some("earlybird") { 2.0 } else { 1.0 };
            self.sides[side].team[idx].sleep_elapsed += inc;
        }
        self.sides[side].team[idx].moves_since_entry += 1.0;
        let actor = self.sides[side].team[idx].index;
        self.record(Action { side: side_key(id.side).into(), slot: id.slot.map(|s| s as i32), actor, kind: "cant".into(), id: Some(eff), target: None, spread: false, missed: false, order: 0, phase: String::new() });
    }
    fn h_faint(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        let m = &mut self.sides[side].team[idx];
        m.fainted = true;
        m.hp = 0.0;
        m.status = String::new();
    }
    fn h_damage(&mut self, a: &[String], tags: &BTreeMap<String, String>) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        if tags.get("from").map(|s| s.as_str()) == Some("psn") && self.sides[side].team[idx].status == "tox" {
            self.sides[side].team[idx].tox_stage = (self.sides[side].team[idx].tox_stage + 1.0).min(15.0);
        }
        if tags.get("from").map(|s| s.as_str()) == Some("confusion") {
            self.sides[side].team[idx].moves_since_entry += 1.0;
            let id = parse_ident(&a[0]).unwrap();
            let actor = self.sides[side].team[idx].index;
            self.record(Action { side: side_key(id.side).into(), slot: id.slot.map(|s| s as i32), actor, kind: "cant".into(), id: Some("confusion".into()), target: None, spread: false, missed: false, order: 0, phase: String::new() });
        }
        let hp = a.get(1).cloned().unwrap_or_default();
        self.apply_hp(side, idx, &hp);
    }
    fn h_fail(&mut self, a: &[String]) {
        let Some(id) = parse_ident(&a[0]) else { return };
        let Some(slot) = id.slot else { return };
        let side = id.side as usize;
        if self.sides[side].active[slot as usize].is_none() { return; }
        let idx = self.mon(&a[0], None);
        let m = &self.sides[side].team[idx];
        let last_ok = self.last_mover == Some(m.index)
            && m.stall.is_some()
            && m.stall.as_ref().and_then(|s| s.get("turn")).and_then(|v| v.as_i64()) == Some(self.turn)
            && m.last_move_turn == Some(self.turn as f64)
            && m.last_move.as_ref().map(|lm| stall_adds(lm)).unwrap_or(false);
        if last_ok {
            let lm = self.sides[side].team[idx].last_move.clone().unwrap_or_default();
            if stall_rolls(&lm) {
                self.sides[side].team[idx].stall = None;
            } else {
                let before = self.sides[side].team[idx].stall.as_ref().and_then(|s| s.get("before").cloned()).unwrap_or(Value::Null);
                self.sides[side].team[idx].stall = if before.is_null() { None } else { Some(before) };
            }
        }
    }
    fn h_heal(&mut self, a: &[String], tags: &BTreeMap<String, String>) {
        if let Some(f) = tags.get("from") {
            if let Some(item) = f.strip_prefix("item: ") {
                if tags.get("of").map(|o| o == &a[0]).unwrap_or(true) {
                    self.explain(&a[0], &Fits::Item(to_id(item)), true);
                }
            }
        }
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        let hp = a.get(1).cloned().unwrap_or_default();
        self.apply_hp(side, idx, &hp);
        if tags.get("from").map(|f| f.as_str()) == Some("move: Revival Blessing") {
            self.sides[side].team[idx].revived = true;
        }
    }
    fn h_sethp(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        let hp = a.get(1).cloned().unwrap_or_default();
        self.apply_hp(side, idx, &hp);
    }
    fn h_status(&mut self, a: &[String], tags: &BTreeMap<String, String>) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        let st = a.get(1).cloned().unwrap_or_default();
        self.sides[side].team[idx].status = st.clone();
        if st == "tox" { self.sides[side].team[idx].tox_stage = 0.0; }
        if st == "slp" {
            self.sides[side].team[idx].sleep_elapsed = 0.0;
            self.sides[side].team[idx].sleep_rest = tags.get("from").map(|f| f.as_str()) == Some("move: Rest");
        }
    }
    fn h_curestatus(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        self.sides[side].team[idx].status = String::new();
    }
    fn h_cureteam(&mut self, a: &[String]) {
        let side = self.side_of(&a[0]);
        for m in &mut self.sides[side].team { m.status = String::new(); }
    }
    fn h_boost(&mut self, a: &[String], d: i32) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        let key = a.get(1).cloned().unwrap_or_default();
        let n: i32 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        let b = self.sides[side].team[idx].boosts.entry(key).or_insert(0);
        *b = if d > 0 { (*b + n).min(6) } else { (*b - n).max(-6) };
    }
    fn h_setboost(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        let key = a.get(1).cloned().unwrap_or_default();
        let n: i32 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        self.sides[side].team[idx].boosts.insert(key, n);
    }
    fn h_clearboost(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        for k in BOOSTS { self.sides[side].team[idx].boosts.insert(k.into(), 0); }
    }
    fn h_clearallboost(&mut self) {
        for s in 0..2 {
            for i in 0..2 {
                if let Some(idx) = self.sides[s].active[i] {
                    for k in BOOSTS { self.sides[s].team[idx as usize].boosts.insert(k.into(), 0); }
                }
            }
        }
    }
    fn h_clear_neg(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        for k in BOOSTS { let b = self.sides[side].team[idx].boosts.entry(k.into()).or_insert(0); *b = (*b).max(0); }
    }
    fn h_clear_pos(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        for k in BOOSTS { let b = self.sides[side].team[idx].boosts.entry(k.into()).or_insert(0); *b = (*b).min(0); }
    }
    fn h_invertboost(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        for k in BOOSTS { let b = self.sides[side].team[idx].boosts.entry(k.into()).or_insert(0); *b = -*b; }
    }
    fn h_copyboost(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        let src = self.mon(&a[1], None);
        let sside = self.mon_side(&a[1]);
        let b = self.sides[sside].team[src].boosts.clone();
        self.sides[side].team[idx].boosts = b;
    }
    fn h_swapboost(&mut self, a: &[String]) {
        let ia = self.mon(&a[0], None);
        let sa = self.mon_side(&a[0]);
        let ib = self.mon(&a[1], None);
        let sb = self.mon_side(&a[1]);
        let stats: Vec<String> = match a.get(2) {
            Some(s) if !s.starts_with('[') && !s.is_empty() => s.split(", ").map(|x| x.to_string()).collect(),
            _ => BOOSTS.iter().map(|s| s.to_string()).collect(),
        };
        for k in stats {
            let va = *self.sides[sa].team[ia].boosts.get(&k).unwrap_or(&0);
            let vb = *self.sides[sb].team[ib].boosts.get(&k).unwrap_or(&0);
            self.sides[sa].team[ia].boosts.insert(k.clone(), vb);
            self.sides[sb].team[ib].boosts.insert(k, va);
        }
    }
    fn h_weather(&mut self, a: &[String], tags: &BTreeMap<String, String>) {
        if a[0] == "none" {
            self.field.weather = None;
        } else if !tags.contains_key("upkeep") {
            self.field.weather = Some(json!({"id": to_id(&a[0]), "since": self.turn}));
        }
    }
    fn h_fieldstart(&mut self, a: &[String]) {
        let id = effect_id(&a[0]);
        if id.ends_with("terrain") {
            self.field.terrain = Some(json!({"id": id, "since": self.turn}));
        } else {
            self.field.pseudo.insert(id, json!({"since": self.turn}));
        }
    }
    fn h_fieldactivate(&mut self, a: &[String]) {
        if effect_id(&a[0]) == "fairylock" {
            self.field.pseudo.insert("fairylock".into(), json!({"since": self.turn, "endsAtUpkeep": self.turn + 1}));
        }
    }
    fn h_fieldend(&mut self, a: &[String]) {
        let id = effect_id(&a[0]);
        if self.field.terrain.as_ref().and_then(|t| t.get("id")).and_then(|v| v.as_str()) == Some(id.as_str()) {
            self.field.terrain = None;
        }
        self.field.pseudo.remove(&id);
    }
    fn h_sidestart(&mut self, a: &[String]) {
        let side = self.side_of(&a[0]);
        let id = effect_id(&a[1]);
        let old = self.sides[side].conditions.get(&id).cloned();
        let since = old.as_ref().and_then(|o| o.get("since")).cloned().unwrap_or(json!(self.turn));
        let layers = old.as_ref().and_then(|o| o.get("layers")).and_then(|v| v.as_i64()).unwrap_or(0) + 1;
        self.sides[side].conditions.insert(id, json!({"since": since, "layers": layers}));
    }
    fn h_sideend(&mut self, a: &[String]) {
        let side = self.side_of(&a[0]);
        let id = effect_id(&a[1]);
        self.sides[side].conditions.remove(&id);
    }
    fn h_swapsideconditions(&mut self) {
        let a = std::mem::take(&mut self.sides[0].conditions);
        let b = std::mem::take(&mut self.sides[1].conditions);
        self.sides[0].conditions = b;
        self.sides[1].conditions = a;
    }
    fn h_start(&mut self, a: &[String], tags: &BTreeMap<String, String>) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        let effect = effect_id(&a[1]);
        if effect == "typechange" {
            if tags.get("from").map(|f| f.as_str()) == Some("move: Reflect Type") {
                if let Some(of) = tags.get("of") {
                    let ti = self.mon(of, None);
                    let ts = self.mon_side(of);
                    let types = self.sides[ts].team[ti].types.clone().or_else(|| {
                        self.sides[ts].team[ti].forme.clone().and_then(|f| species_of(&f)).map(|s| s.types.iter().map(|t| t.name().to_string()).collect())
                    });
                    self.sides[side].team[idx].types = types;
                    return;
                }
            }
            self.sides[side].team[idx].types = Some(a.get(2).map(|s| s.split('/').map(|x| x.to_string()).collect()).unwrap_or_default());
            return;
        }
        if effect == "typeadd" {
            let mut types = self.sides[side].team[idx].types.clone().or_else(|| {
                self.sides[side].team[idx].forme.clone().and_then(|f| species_of(&f)).map(|s| s.types.iter().map(|t| t.name().to_string()).collect())
            }).unwrap_or_default();
            if let Some(x) = a.get(2) { types.push(x.clone()); }
            self.sides[side].team[idx].types = Some(types);
            return;
        }
        let (id, mut value) = Tracker::volatile(&a[1], &[("since", json!(self.turn))]);
        if tags.contains_key("partiallytrapped") {
            if let Some(v) = value.as_object_mut() { v.insert("hiddenDuration".into(), json!(true)); }
        }
        if id == "confusion" {
            let last_move = self.history.last().map(|h| h.actions.iter().rev().find(|a| a.kind == "move").and_then(|a| a.id.clone())).flatten();
            if let Some(v) = value.as_object_mut() {
                v.insert("elapsed".into(), json!(0));
                v.insert("minimum".into(), json!(if last_move.as_deref() == Some("axekick") { 3 } else { 2 }));
            }
        }
        self.sides[side].team[idx].volatiles.insert(id, value);
    }
    fn h_end(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        let (id, _) = Tracker::volatile(&a[1], &[]);
        if id == "illusion" { return; }
        if id == "typechange" { self.sides[side].team[idx].types = None; }
        self.sides[side].team[idx].volatiles.remove(&id);
    }
    fn h_singleturn(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        let (id, value) = Tracker::volatile(&a[1], &[("since", json!(self.turn)), ("singleturn", json!(true))]);
        if SIDE_SINGLETURN.contains(&id.as_str()) {
            self.sides[side].conditions.insert(id, json!({"since": self.turn, "layers": 1, "singleturn": true}));
        } else {
            self.sides[side].team[idx].volatiles.insert(id, value);
        }
    }
    fn h_singlemove(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        let (id, value) = Tracker::volatile(&a[1], &[("since", json!(self.turn)), ("singlemove", json!(true))]);
        self.sides[side].team[idx].volatiles.insert(id, value);
    }
    fn h_mustrecharge(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        self.sides[side].team[idx].volatiles.insert("mustrecharge".into(), json!({"since": self.turn, "singlemove": true}));
    }
    fn h_prepare(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        self.sides[side].team[idx].volatiles.insert("twoturnmove".into(), json!({"since": self.turn, "move": to_id(a.get(1).map(|s| s.as_str()).unwrap_or(""))}));
    }
    fn h_enditem(&mut self, a: &[String]) {
        if let Some(it) = a.get(1) {
            self.explain(&a[0], &Fits::Item(to_id(it)), true);
        }
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        self.sides[side].team[idx].item = Some(String::new());
        self.sides[side].team[idx].item_gone = true;
    }
    fn h_item(&mut self, a: &[String], tags: &BTreeMap<String, String>) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        self.sides[side].team[idx].item = Some(to_id(a.get(1).map(|s| s.as_str()).unwrap_or("")));
        self.sides[side].team[idx].item_gone = false;
        if tags.get("from").map(|f| f.as_str()) == Some("ability: Magician") {
            if let Some(of) = tags.get("of") {
                let oi = self.mon(of, None);
                let os = self.mon_side(of);
                self.sides[os].team[oi].item = Some(String::new());
                self.sides[os].team[oi].item_gone = true;
            }
        }
    }
    fn h_ability(&mut self, a: &[String], tags: &BTreeMap<String, String>) {
        let ability = to_id(a.get(1).map(|s| s.as_str()).unwrap_or(""));
        if !tags.contains_key("from") {
            self.explain(&a[0], &Fits::Ability(ability.clone()), true);
        }
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        self.sides[side].team[idx].ability = Some(ability);
    }
    fn h_activate(&mut self, a: &[String], tags: &BTreeMap<String, String>) {
        let effect = effect_id(a.get(1).map(|s| s.as_str()).unwrap_or(""));
        if effect == "feint" || tags.contains_key("broken") {
            let idx = self.mon(&a[0], None);
            let side = self.mon_side(&a[0]);
            self.sides[side].team[idx].stall = None;
        }
        if effect == "confusion" {
            let idx = self.mon(&a[0], None);
            let side = self.mon_side(&a[0]);
            if let Some(v) = self.sides[side].team[idx].volatiles.get_mut("confusion").and_then(|v| v.as_object_mut()) {
                let cur = v.get("elapsed").and_then(|x| x.as_i64()).unwrap_or(0);
                v.insert("elapsed".into(), json!(cur + 1));
            }
        }
        if effect == "skillswap" {
            if let Some(of) = tags.get("of") {
                let ia = self.mon(&a[0], None);
                let sa = self.mon_side(&a[0]);
                let ib = self.mon(of, None);
                let sb = self.mon_side(of);
                let named = a.get(2).map(|s| !s.is_empty() && !s.starts_with('[')).unwrap_or(false);
                if named {
                    let na = to_id(a.get(2).map(|s| s.as_str()).unwrap_or(""));
                    let nb = to_id(a.get(3).map(|s| s.as_str()).unwrap_or(""));
                    self.sides[sa].team[ia].ability = Some(na);
                    self.sides[sb].team[ib].ability = Some(nb);
                } else {
                    let aa = self.sides[sa].team[ia].ability.clone();
                    let ab = self.sides[sb].team[ib].ability.clone();
                    self.sides[sa].team[ia].ability = ab;
                    self.sides[sb].team[ib].ability = aa;
                }
            }
        } else if effect == "mummy" || effect == "lingeringaroma" {
            if let Some(x) = a.get(2) {
                if parse_ident(x).is_some() {
                    let idx = self.mon(x, None);
                    let side = self.mon_side(x);
                    self.sides[side].team[idx].ability = Some(effect.clone());
                }
            }
        } else if effect == "symbiosis" {
            if let Some(of) = tags.get("of") {
                let gi = self.mon(&a[0], None);
                let gs = self.mon_side(&a[0]);
                self.sides[gs].team[gi].item = Some(String::new());
                self.sides[gs].team[gi].item_gone = true;
                let ri = self.mon(of, None);
                let rs = self.mon_side(of);
                self.sides[rs].team[ri].item = Some(to_id(a.get(2).map(|s| s.as_str()).unwrap_or("")));
                self.sides[rs].team[ri].item_gone = false;
            }
        }
    }
    fn h_endability(&mut self, a: &[String]) {
        let idx = self.mon(&a[0], None);
        let side = self.mon_side(&a[0]);
        self.sides[side].team[idx].volatiles.insert("gastroacid".into(), json!({"since": self.turn}));
    }
    fn h_transform(&mut self, a: &[String]) {
        let mi = self.mon(&a[0], None);
        let ms = self.mon_side(&a[0]);
        let ti = self.mon(&a[1], None);
        let ts = self.mon_side(&a[1]);
        let forme = self.sides[ts].team[ti].forme.clone();
        let boosts = self.sides[ts].team[ti].boosts.clone();
        self.sides[ms].team[mi].volatiles.insert("transform".into(), json!({"since": self.turn, "into": forme}));
        self.sides[ms].team[mi].forme = self.sides[ts].team[ti].forme.clone();
        self.sides[ms].team[mi].boosts = boosts;
    }
    fn h_swap(&mut self, a: &[String]) {
        let id = parse_ident(&a[0]).unwrap();
        let side = id.side as usize;
        let to = a.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
        let idx = self.mon(&a[0], None);
        let from = self.sides[side].team[idx].slot.unwrap_or(0);
        let fa = self.sides[side].active[from as usize];
        let ta = self.sides[side].active[to as usize];
        self.sides[side].active[from as usize] = ta;
        self.sides[side].active[to as usize] = fa;
        for s in [from, to] {
            if let Some(i) = self.sides[side].active[s as usize] {
                self.sides[side].team[i as usize].slot = Some(s);
            }
        }
    }
    fn h_win(&mut self, a: &[String]) {
        self.ended = true;
        self.winner = a.first().cloned();
    }

    // ---- Illusion-aware identification ----------------------------------------------------
    fn explain(&mut self, ident: &str, f: &Fits, applicable: bool) {
        let Some(id) = parse_ident(ident) else { return };
        let side = id.side as usize;
        if !applicable || self.sides[side].sheet != Some(true) || id.slot.is_none() {
            return;
        }
        let idx = self.mon(ident, None);
        let illusion = |m: &Mon| m.ability.as_deref() == Some("illusion") || m.base_ability.as_deref() == Some("illusion");
        if fits(&self.sides[side].team[idx], f) {
            let any_other = (0..self.sides[side].team.len()).any(|i| {
                let m = &self.sides[side].team[i];
                i != idx && illusion(m) && (!m.fainted || m.identity_confirmed != Some(true)) && fits(m, f)
            });
            if !any_other {
                self.sides[side].team[idx].identity_confirmed = Some(true);
            }
            return;
        }
        let mon_is_illusion = illusion(&self.sides[side].team[idx]);
        let candidates: Vec<usize> = (0..self.sides[side].team.len()).filter(|&i| {
            let m = &self.sides[side].team[i];
            i != idx && !m.fainted && fits(m, f) && (illusion(m) || mon_is_illusion)
        }).collect();
        let named = self.sides[side].names.get(&id.name).copied().map(|n| n as usize);
        if candidates.len() == 1 {
            let c = candidates[0];
            self.reveal(ident, c);
            self.sides[side].team[c].identity_confirmed = Some(true);
        } else if let Some(n) = named {
            if candidates.contains(&n) && mon_is_illusion {
                self.reveal(ident, n);
                self.sides[side].team[n].identity_confirmed = Some(true);
            }
        }
    }

    fn incoming(&mut self, ident: &str, details: Option<&str>, hp_text: Option<&str>) -> usize {
        let id = parse_ident(ident).unwrap();
        let side = id.side as usize;
        let named = self.mon(ident, details);
        let revived = std::mem::take(&mut self.sides[side].team[named].revived);
        if revived && (self.sides[side].team[named].slot.is_none() || self.sides[side].team[named].slot == id.slot.map(|s| s as i32)) {
            return named;
        }
        let hp = hp_text.and_then(parse_hp);
        let fits_hp = |m: &Mon| -> bool {
            if m.fainted { return false; }
            let Some(hp) = &hp else { return true };
            if !m.seen { return true; }
            if hp.fraction < m.hp - 0.006 { return false; }
            hp.fraction <= m.hp + 0.006 || m.ability.as_deref() == Some("regenerator") || m.ability.is_none()
        };
        if self.sides[side].team[named].slot.is_none() && fits_hp(&self.sides[side].team[named]) {
            return named;
        }
        let can_illusion = |m: &Mon| -> bool {
            m.ability.as_deref() == Some("illusion")
                || m.base_ability.as_deref() == Some("illusion")
                || (m.ability.is_none() && m.species.as_ref().and_then(|s| species_of(s)).map(|s| s.abilities.iter().any(|a| a == "Illusion")).unwrap_or(false))
        };
        let slot = id.slot.map(|s| s as i32);
        let misplaced: Option<usize> = (0..self.sides[side].team.len()).find(|&i| {
            let m = &self.sides[side].team[i];
            can_illusion(m) && m.slot.is_some() && m.slot != slot && m.confused_with.is_some()
                && self.sides[side].team[m.confused_with.unwrap() as usize].slot.is_none()
        });
        if let Some(mp) = misplaced {
            if named == mp || self.sides[side].team[named].slot == slot {
                let partner = self.sides[side].team[mp].confused_with.unwrap() as usize;
                let pname = self.sides[side].team[partner].name.clone().unwrap_or_default();
                let slotc = b"ab"[self.sides[side].team[mp].slot.unwrap() as usize] as char;
                let ident2 = format!("{}{}: {}", side_key(id.side), slotc, pname);
                self.reveal(&ident2, partner);
                if self.sides[side].team[named].slot.is_none() && fits_hp(&self.sides[side].team[named]) {
                    return named;
                }
            }
        }
        let benched = |t: &Tracker| -> Option<usize> {
            let s = &t.sides[side].team;
            (0..s.len()).find(|&i| i != named && can_illusion(&s[i]) && s[i].slot.is_none() && fits_hp(&s[i]))
        };
        let mut user = benched(self);
        if self.sides[side].team[named].slot.is_none()
            && !fits_hp(&self.sides[side].team[named])
            && self.sides[side].team[named].identity_confirmed != Some(true)
        {
            if let Some(u) = user {
                let snap_seen_false = self.sides[side].team[named].snapshot.as_ref().and_then(|s| s.get("seen")).and_then(|v| v.as_bool()) == Some(false);
                if !self.sides[side].team[u].seen && snap_seen_false {
                    self.swap_observations(named, u, side);
                    self.sides[side].team[u].seen = true;
                    self.sides[side].team[u].confused_with = Some(named as i32);
                    return named;
                }
            }
        }
        if user.is_none() {
            let fallen: Option<usize> = (0..self.sides[side].team.len()).find(|&i| {
                let m = &self.sides[side].team[i];
                can_illusion(m) && m.fainted && m.confused_with.is_some()
                    && !self.sides[side].team[m.confused_with.unwrap() as usize].fainted
                    && self.sides[side].team[m.confused_with.unwrap() as usize].slot.is_none()
            });
            if let Some(f) = fallen {
                let partner = self.sides[side].team[f].confused_with.unwrap() as usize;
                self.swap_observations(f, partner, side);
                if let Some(fs) = self.sides[side].team[f].slot {
                    self.sides[side].active[fs as usize] = Some(partner as i32);
                    self.sides[side].team[partner].slot = Some(fs);
                    self.sides[side].team[f].slot = None;
                }
                user = benched(self);
            }
        }
        if let Some(u) = user {
            if self.sides[side].team[named].slot.is_some() {
                self.sides[side].team[u].confused_with = Some(named as i32);
            }
            return u;
        }
        if self.sides[side].team[named].slot.is_some() {
            let old = self.sides[side].team[named].slot.unwrap();
            let real = (0..self.sides[side].team.len()).find(|&i| {
                let m = &self.sides[side].team[i];
                i != named && can_illusion(m) && !m.fainted && (m.slot.is_none() || m.slot == slot)
            }).or_else(|| (0..self.sides[side].team.len()).find(|&i| {
                let m = &self.sides[side].team[i];
                i != named && can_illusion(m) && m.identity_confirmed != Some(true) && (m.slot.is_none() || m.slot == slot)
            }));
            if let Some(real) = real {
                if self.sides[side].team[real].slot.is_some() && self.sides[side].team[real].slot != Some(old) {
                    let rs = self.sides[side].team[real].slot.unwrap() as usize;
                    self.sides[side].active[rs] = None;
                }
                self.sides[side].team[real].slot = None;
                let nm = self.sides[side].team[named].name.clone().unwrap_or_default();
                let slotc = b"ab"[old as usize] as char;
                let ident2 = format!("{}{}: {}", side_key(id.side), slotc, nm);
                self.reveal(&ident2, real);
            } else if old != slot.unwrap_or(-1) {
                panic!("Conflicting simultaneous identity at {ident}");
            } else {
                return named;
            }
            self.sides[side].team[named].slot = None;
        }
        named
    }

    fn reveal(&mut self, ident: &str, real: usize) {
        let Some(id) = parse_ident(ident) else { return };
        let side = id.side as usize;
        let slot = id.slot.unwrap_or(0) as usize;
        let fake = self.sides[side].active[slot].map(|i| i as usize).unwrap_or(real);
        let partner = self.sides[side].team[real].confused_with.map(|i| i as usize);
        if fake != real {
            if let Some(rs) = self.sides[side].team[real].slot {
                if rs as usize != slot {
                    if let Some(p) = partner {
                        if p != fake && self.sides[side].team[p].slot.is_none() {
                            let pn = self.sides[side].team[p].name.clone().unwrap_or_default();
                            let slotc = b"ab"[rs as usize] as char;
                            let ident2 = format!("{}{}: {}", side_key(id.side), slotc, pn);
                            self.reveal(&ident2, p);
                        }
                    }
                }
            }
        }
        for m in &mut self.sides[side].team { m.confused_with = None; }
        let real_slot = self.sides[side].team[real].slot;
        if fake != real && real_slot.is_some() && real_slot != Some(slot as i32) {
            let other = real_slot.unwrap() as usize;
            let fields = ["hp", "exact", "status", "fainted", "sleep_elapsed", "sleep_rest", "entered_turn", "moves_since_entry", "stall", "tox_stage", "last_move", "last_move_turn"];
            let copy = |m: &Mon| -> BTreeMap<&'static str, Value> {
                let mut o = BTreeMap::new();
                o.insert("hp", json!(m.hp));
                o.insert("exact", m.exact.map(|e| json!(e)).unwrap_or(Value::Null));
                o.insert("status", json!(m.status));
                o.insert("fainted", json!(m.fainted));
                o.insert("sleep_elapsed", json!(m.sleep_elapsed));
                o.insert("sleep_rest", json!(m.sleep_rest));
                o.insert("entered_turn", json!(m.entered_turn));
                o.insert("moves_since_entry", json!(m.moves_since_entry));
                o.insert("stall", m.stall.clone().unwrap_or(Value::Null));
                o.insert("tox_stage", json!(m.tox_stage));
                o.insert("last_move", opt(&m.last_move));
                o.insert("last_move_turn", m.last_move_turn.map(|t| json!(t)).unwrap_or(Value::Null));
                let _ = fields;
                o
            };
            let a = copy(&self.sides[side].team[fake]);
            let b = copy(&self.sides[side].team[real]);
            apply_map(&mut self.sides[side].team[real], &a);
            apply_map(&mut self.sides[side].team[fake], &b);
            let fb = std::mem::take(&mut self.sides[side].team[fake].boosts);
            let rb = std::mem::replace(&mut self.sides[side].team[real].boosts, fb);
            self.sides[side].team[fake].boosts = rb;
            let fv = std::mem::take(&mut self.sides[side].team[fake].volatiles);
            let rv = std::mem::replace(&mut self.sides[side].team[real].volatiles, fv);
            self.sides[side].team[fake].volatiles = rv;
            if !self.sides[side].team[fake].me_evolved {
                self.sides[side].team[fake].forme = self.sides[side].team[fake].species.clone();
            }
            self.sides[side].active[slot] = Some(real as i32);
            self.sides[side].active[other] = Some(fake as i32);
            self.sides[side].team[real].slot = Some(slot as i32);
            self.sides[side].team[fake].slot = Some(other as i32);
            let m1 = BTreeMap::from([(fake as i32, real as i32)]);
            self.reassign_actions(side, Some(slot as i32), &m1);
            let m2 = BTreeMap::from([(real as i32, fake as i32)]);
            self.reassign_actions(side, Some(other as i32), &m2);
        } else if fake != real {
            let o = self.sides[side].team[fake].clone();
            {
                let r = &mut self.sides[side].team[real];
                r.boosts = o.boosts.clone();
                r.volatiles = o.volatiles.clone();
                r.hp = o.hp;
                r.status = o.status.clone();
                r.sleep_elapsed = o.sleep_elapsed;
                r.sleep_rest = o.sleep_rest;
                r.entered_turn = o.entered_turn;
                r.moves_since_entry = o.moves_since_entry;
                r.stall = o.stall.clone();
                r.tox_stage = o.tox_stage;
                r.last_move = o.last_move.clone();
                r.last_move_turn = o.last_move_turn;
            }
            let m1 = BTreeMap::from([(fake as i32, real as i32)]);
            self.reassign_actions(side, Some(slot as i32), &m1);
            let before = self.sides[side].team[fake].snapshot.clone().unwrap_or(Value::Null);
            let before_item = before.get("item").cloned().unwrap_or(Value::Null);
            let before_ab = before.get("ability").cloned().unwrap_or(Value::Null);
            if opt(&self.sides[side].team[fake].item) != before_item {
                let it = self.sides[side].team[fake].item.clone();
                let gone = self.sides[side].team[fake].item_gone;
                self.sides[side].team[real].item = it;
                self.sides[side].team[real].item_gone = gone;
            }
            if opt(&self.sides[side].team[fake].ability) != before_ab {
                self.sides[side].team[real].ability = self.sides[side].team[fake].ability.clone();
            }
            let misplaced_faint = self.sides[side].team[real].fainted && !self.sides[side].team[fake].fainted;
            let slot_fainted = self.sides[side].team[fake].fainted;
            let f = &mut self.sides[side].team[fake];
            f.seen = before.get("seen").and_then(|v| v.as_bool()).unwrap_or(false);
            f.hp = before.get("hp").and_then(|v| v.as_f64()).unwrap_or(1.0);
            f.exact = before.get("exact").and_then(|v| v.as_array()).map(|a| [a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0)]);
            f.status = before.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
            f.fainted = before.get("fainted").and_then(|v| v.as_bool()).unwrap_or(false);
            f.item = before.get("item").and_then(|v| v.as_str()).map(|s| s.to_string());
            f.item_gone = before.get("itemGone").and_then(|v| v.as_bool()).unwrap_or(false);
            f.ability = before.get("ability").and_then(|v| v.as_str()).map(|s| s.to_string());
            f.sleep_elapsed = before.get("sleepElapsed").and_then(|v| v.as_f64()).unwrap_or(0.0);
            f.sleep_rest = before.get("sleepRest").and_then(|v| v.as_bool()).unwrap_or(false);
            f.entered_turn = 0.0;
            f.moves_since_entry = 0.0;
            f.stall = None;
            f.tox_stage = 0.0;
            f.last_move = before.get("lastMove").and_then(|v| v.as_str()).map(|s| s.to_string());
            f.last_move_turn = before.get("lastMoveTurn").and_then(|v| v.as_f64());
            f.slot = None;
            for k in BOOSTS { f.boosts.insert(k.into(), 0); }
            f.volatiles.clear();
            self.sides[side].team[real].fainted = slot_fainted;
            if misplaced_faint {
                self.sides[side].team[fake].fainted = true;
                self.sides[side].team[fake].hp = 0.0;
                self.sides[side].team[fake].status = String::new();
            }
            if let Some(rs) = self.sides[side].team[real].slot {
                if rs as usize != slot { self.sides[side].active[rs as usize] = None; }
            }
            self.sides[side].active[slot] = Some(real as i32);
            self.sides[side].team[real].slot = Some(slot as i32);
            self.sides[side].team[real].seen = true;
        }
    }

    fn set_active(&mut self, side: usize, slot: usize, mon: usize, details: &str, hp_text: &str, from: Option<&str>) {
        let out = self.sides[side].active[slot];
        let mut carried_boosts: Option<BTreeMap<String, i32>> = None;
        let mut carried_vols: Option<Omap<Value>> = None;
        let mut carried_stall: Option<Value> = None;
        if let Some(out) = out {
            let old = out as usize;
            if from == Some("Baton Pass") {
                carried_boosts = Some(self.sides[side].team[old].boosts.clone());
                carried_vols = Some(self.sides[side].team[old].volatiles.iter().filter(|(k, _)| !volatile_no_copy(k)).map(|(k, v)| (k.clone(), v.clone())).collect());
                carried_stall = self.sides[side].team[old].stall.clone();
            } else if from == Some("Shed Tail") {
                carried_vols = Some(self.sides[side].team[old].volatiles.iter().filter(|(k, _)| *k == "substitute").map(|(k, v)| (k.clone(), v.clone())).collect());
            }
            let old_m = &mut self.sides[side].team[old];
            old_m.slot = None;
            for k in BOOSTS { old_m.boosts.insert(k.into(), 0); }
            old_m.volatiles.clear();
            old_m.types = None;
            let reset_ab = if old_m.me_evolved {
                old_m.forme.clone().and_then(|f| species_of(&f)).and_then(|s| s.abilities.first().map(|a| to_id(a)))
            } else {
                old_m.base_ability.clone()
            };
            old_m.ability = reset_ab;
            if !old_m.me_evolved { old_m.forme = old_m.species.clone(); }
        }
        self.sides[side].team[mon].snapshot = Some(Tracker::snapshot_of(&self.sides[side].team[mon]));
        if let Some(s) = self.sides[side].team[mon].slot {
            if s as usize != slot { self.sides[side].active[s as usize] = None; }
        }
        self.sides[side].active[slot] = Some(mon as i32);
        self.sides[side].team[mon].slot = Some(slot as i32);
        self.sides[side].team[mon].seen = true;
        let shown = species_of(details);
        let cur_species = self.sides[side].team[mon].species.clone();
        let base_mismatch = shown.map(|sh| cur_species.as_ref().and_then(|s| base_species_of_name(s)).as_deref() != Some(sh.base_species.as_str())).unwrap_or(false);
        self.sides[side].team[mon].forme = if base_mismatch { cur_species.clone() } else { shown.map(|s| s.name.clone()) };
        if let Some(f) = self.sides[side].team[mon].forme.clone() {
            if species_of(&f).map(|s| s.is_mega).unwrap_or(false) {
                self.sides[side].team[mon].me_evolved = true;
            }
        }
        self.sides[side].team[mon].boosts = carried_boosts.unwrap_or_else(|| BOOSTS.iter().map(|b| (b.to_string(), 0)).collect());
        self.sides[side].team[mon].volatiles = carried_vols.unwrap_or_default();
        self.sides[side].team[mon].types = None;
        self.sides[side].team[mon].entered_turn = self.turn as f64;
        self.sides[side].team[mon].moves_since_entry = 0.0;
        self.sides[side].team[mon].stall = if from == Some("Baton Pass") { carried_stall } else { None };
        self.sides[side].team[mon].tox_stage = 0.0;
        let hp = hp_text.to_string();
        self.apply_hp(side, mon, &hp);
    }

    pub fn observe_request(&mut self, req: &Value) {
        self.request = Some(req.clone());
        if req.get("wait").and_then(|v| v.as_bool()).unwrap_or(false) {
            return;
        }
        let side = self.me_side();
        let team_preview = req.get("teamPreview").and_then(|v| v.as_bool()).unwrap_or(false);
        let entries = req.get("side").and_then(|s| s.get("pokemon")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
        if team_preview {
            for (i, entry) in entries.iter().enumerate() {
                let details = entry.get("details").and_then(|v| v.as_str()).unwrap_or("");
                let Some(sp) = species_of(details) else { continue };
                let pos = self.sides[side].team.iter().position(|m| m.species.as_ref().and_then(|s| base_species_of_name(s)).as_deref() == Some(sp.base_species.as_str()));
                let idx = match pos {
                    Some(p) => p,
                    None => {
                        let mut mon = Mon::new(i as i32, None, None, None, &[], None, "");
                        mon.species = Some(sp.name.clone());
                        mon.forme = Some(sp.name.clone());
                        if self.sides[side].team.len() <= i {
                            self.sides[side].team.push(mon);
                        } else {
                            self.sides[side].team[i] = mon;
                        }
                        i
                    }
                };
                let name = entry.get("ident").and_then(|v| v.as_str()).unwrap_or("").get(4..).unwrap_or("").to_string();
                self.sides[side].team[idx].name = Some(name.clone());
                self.sides[side].names.insert(name, idx as i32);
            }
        }
        let active = req.get("active").and_then(|v| v.as_array()).cloned();
        let active_count = active.as_ref().map(|a| a.len()).unwrap_or_else(|| req.get("forceSwitch").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0));
        let observed: Vec<Option<Mon>> = (0..2).map(|slot| self.sides[side].active[slot].map(|i| self.sides[side].team[i as usize].clone())).collect();
        for (position, entry) in entries.iter().enumerate() {
            let ident = entry.get("ident").and_then(|v| v.as_str()).unwrap_or("");
            let name = ident.get(4..).unwrap_or("").to_string();
            let Some(index) = self.sides[side].names.get(&name).copied() else { panic!("Request names an untracked Pokemon: {ident}") };
            let idx = index as usize;
            if !team_preview { self.sides[side].team[idx].brought = Some(true); }
            let cond = entry.get("condition").and_then(|v| v.as_str()).unwrap_or("");
            let hp = parse_hp(cond).unwrap_or(Hp { fraction: 1.0, exact: None, status: String::new(), fainted: false });
            let m = &mut self.sides[side].team[idx];
            m.hp = hp.fraction;
            m.exact = hp.exact;
            m.status = hp.status;
            m.fainted = hp.fainted;
            m.item = Some(to_id(entry.get("item").and_then(|v| v.as_str()).unwrap_or("")));
            let ability = entry.get("ability").and_then(|v| v.as_str()).or_else(|| entry.get("baseAbility").and_then(|v| v.as_str())).unwrap_or("");
            m.ability = Some(to_id(ability));
            m.base_ability = Some(to_id(entry.get("baseAbility").and_then(|v| v.as_str()).unwrap_or("")));
            m.moves = entry.get("moves").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(to_id).collect()).unwrap_or_default();
            m.stats = entry.get("stats").cloned();
            m.commanding = Some(entry.get("commanding").and_then(|v| v.as_bool()).unwrap_or(false));
            if !team_preview && position < active_count && self.sides[side].active[position] != Some(index) {
                let wrong = self.sides[side].active[position];
                if let Some(w) = wrong {
                    if w != index {
                        let fake = w as usize;
                        for k in BOOSTS { self.sides[side].team[fake].boosts.insert(k.into(), 0); }
                        self.sides[side].team[fake].volatiles.clear();
                        self.sides[side].team[fake].slot = None;
                    }
                }
                if let Some(Some(o)) = observed.get(position) {
                    let o = o.clone();
                    let m = &mut self.sides[side].team[idx];
                    m.boosts = o.boosts;
                    m.volatiles = o.volatiles;
                    m.sleep_elapsed = o.sleep_elapsed;
                    m.sleep_rest = o.sleep_rest;
                    m.entered_turn = o.entered_turn;
                    m.moves_since_entry = o.moves_since_entry;
                    m.stall = o.stall;
                    m.tox_stage = o.tox_stage;
                    m.last_move = o.last_move;
                    m.last_move_turn = o.last_move_turn;
                }
                if let Some(w) = wrong {
                    if w != index {
                        let map = BTreeMap::from([(w, index)]);
                        self.reassign_actions(side, Some(position as i32), &map);
                    }
                }
                if let Some(s) = self.sides[side].team[idx].slot {
                    if s as usize != position { self.sides[side].active[s as usize] = None; }
                }
                self.sides[side].active[position] = Some(index);
                self.sides[side].team[idx].slot = Some(position as i32);
                if let Some(sp) = species_of(entry.get("details").and_then(|v| v.as_str()).unwrap_or("")) {
                    self.sides[side].team[idx].forme = Some(sp.name.clone());
                }
            }
        }
        if !team_preview {
            for m in &mut self.sides[side].team {
                if m.brought != Some(true) { m.brought = Some(false); }
            }
        }
    }
}

// ---- module-level helpers ------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Fits {
    Move(String),
    Ability(String),
    Item(String),
}

fn fits(m: &Mon, f: &Fits) -> bool {
    match f {
        Fits::Move(x) => m.moves.iter().any(|v| v == x),
        Fits::Ability(x) => m.base_ability.as_deref() == Some(x.as_str()),
        Fits::Item(x) => m.item.as_deref() == Some(x.as_str()),
    }
}

fn apply_map(m: &mut Mon, o: &BTreeMap<&'static str, Value>) {
    m.hp = o["hp"].as_f64().unwrap_or(m.hp);
    m.exact = o["exact"].as_array().map(|a| [a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0)]);
    m.status = o["status"].as_str().unwrap_or("").to_string();
    m.fainted = o["fainted"].as_bool().unwrap_or(false);
    m.sleep_elapsed = o["sleep_elapsed"].as_f64().unwrap_or(0.0);
    m.sleep_rest = o["sleep_rest"].as_bool().unwrap_or(false);
    m.entered_turn = o["entered_turn"].as_f64().unwrap_or(0.0);
    m.moves_since_entry = o["moves_since_entry"].as_f64().unwrap_or(0.0);
    m.stall = if o["stall"].is_null() { None } else { Some(o["stall"].clone()) };
    m.tox_stage = o["tox_stage"].as_f64().unwrap_or(0.0);
    m.last_move = o["last_move"].as_str().map(|s| s.to_string());
    m.last_move_turn = o["last_move_turn"].as_f64();
}

fn volatile_no_copy(id: &str) -> bool {
    matches!(id, "focusenergy" | "laserfocus")
}

fn parse_counted(raw: &str) -> Option<(String, i64)> {
    for p in ["perish", "stockpile", "fallen"] {
        if let Some(n) = raw.strip_prefix(p) {
            if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
                return Some((p.to_string(), n.parse().ok()?));
            }
        }
    }
    None
}

pub fn stall_adds(move_id: &str) -> bool {
    let Some(mid) = dex().move_id(move_id) else { return false };
    use crate::kinds::MoveFx::*;
    matches!(dex().mv(mid).fx, Protect | Detect | SpikyShield | BanefulBunker | KingsShield | SilkTrap | BurningBulwark | Endure | WideGuard | QuickGuard)
}

pub fn stall_rolls(move_id: &str) -> bool {
    stall_adds(move_id) && !matches!(move_id, "quickguard" | "wideguard")
}

fn unpack_sets(packed: &str) -> Vec<Map<String, Value>> {
    packed.split(']').filter(|s| !s.is_empty()).map(|set| {
        let f: Vec<&str> = set.split('|').collect();
        let get = |i: usize| f.get(i).copied().unwrap_or("").to_string();
        let mut m = Map::new();
        m.insert("name".into(), json!(get(0)));
        m.insert("species".into(), json!(get(1)));
        m.insert("item".into(), json!(get(2)));
        m.insert("ability".into(), json!(get(3)));
        let moves: Vec<&str> = f.get(4).copied().unwrap_or("").split(',').collect();
        m.insert("moves".into(), json!(moves));
        m.insert("nature".into(), json!(get(5)));
        m.insert("gender".into(), json!(get(7)));
        m
    }).collect()
}
