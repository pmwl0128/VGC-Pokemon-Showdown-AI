//! Native v3 encoder: the Rust port of `model_train/env/encoder.cjs` (+ `actions.cjs` masks).
//!
//! Turns a native `Tracker` plus the player's own request into the fixed-shape arrays the network
//! consumes (`cats`, `floats`, `switchMap`, and the action masks). Validated against the JS encoder
//! by feeding the same `track.js` fixture to both and comparing every element.
//!
//! Token layout (T = 27): own team 0-5, foe team 6-11, active slots 12-15, own side 16, foe side
//! 17, field 18, own move tokens 19-26. Global ids come from `fixtures/vocab.json.gz`
//! (`adapters/vgczero/parity/vocab.js`), matching `VOCAB.hash`.

use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

use serde_json::Value;

use crate::dex::{dex, Category};
use crate::tracker::{to_id, Mon, Tracker};

pub const T: usize = 27;
pub const C: usize = 17;
pub const F: usize = 56;
const STAT_SCALE: f64 = 300.0;
pub const SLOT_ACTIONS: usize = 45;
const SWITCH_BASE: usize = 40;
pub const PASS: usize = 44;
const BOOSTS: [&str; 7] = ["atk", "def", "spa", "spd", "spe", "accuracy", "evasion"];

fn clamp(x: f64) -> f64 {
    x.max(-1.0).min(1.0)
}

// ---- vocabulary --------------------------------------------------------------------------

struct FieldV {
    offset: i32,
    index: HashMap<String, i32>,
}

pub struct Vocab {
    fields: HashMap<String, FieldV>,
    pub size: i32,
    pub hash: String,
}

static VOCAB: OnceLock<Vocab> = OnceLock::new();

fn vocab() -> &'static Vocab {
    VOCAB.get_or_init(load_vocab)
}

fn load_vocab() -> Vocab {
    let path = crate::data_path("fixtures/vocab.json.gz");
    let bytes = std::fs::read(&path).unwrap_or_else(|_| panic!("vocab fixture missing: {path} (run `node vocab.js`)"));
    let mut text = String::new();
    flate2::read::GzDecoder::new(&bytes[..]).read_to_string(&mut text).unwrap();
    let v: Value = serde_json::from_str(&text).unwrap();
    let mut fields = HashMap::new();
    for f in v["fields"].as_array().unwrap() {
        let name = f["name"].as_str().unwrap().to_string();
        let offset = f["offset"].as_i64().unwrap() as i32;
        let mut index = HashMap::new();
        for (i, val) in f["values"].as_array().unwrap().iter().enumerate() {
            index.insert(val.as_str().unwrap().to_string(), (i as i32) + 1);
        }
        fields.insert(name, FieldV { offset, index });
    }
    Vocab { fields, size: v["size"].as_i64().unwrap() as i32, hash: v["hash"].as_str().unwrap().to_string() }
}

/// Global id for `value` in `field`; empty/none -> the field's unknown id; unseen names throw.
pub fn id(field: &str, value: Option<&str>) -> i32 {
    let f = &vocab().fields[field];
    match value {
        None => f.offset,
        Some(v) if v.is_empty() => f.offset,
        Some(v) => *f.index.get(v).unwrap_or_else(|| panic!("Unknown {field}: {v}")) + f.offset,
    }
}

/// `speciesId(name)`: the toID'd species id, or None.
fn species_id_of(name: &str) -> Option<String> {
    dex().species_id(&to_id(name)).map(|i| dex().sp(i).id.clone())
}

fn species_by_name(name: &str) -> Option<&'static crate::dex::SpeciesData> {
    dex().species_id(&to_id(name)).map(|i| dex().sp(i))
}

// ---- output ------------------------------------------------------------------------------

pub struct Obs {
    pub cats: Vec<i32>,
    pub floats: Vec<f32>,
    pub switch_map: [i8; 6],
    pub kind: String,
}

pub struct Masks {
    pub kind: String,
    pub preview: Option<Vec<u8>>,
    pub mask0: Option<Vec<u8>>,
    pub mask1: Option<Vec<u8>>,
}

// ---- request helpers ---------------------------------------------------------------------

fn get<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get(k)
}
fn as_str<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(|x| x.as_str())
}
fn as_arr<'a>(v: &'a Value, k: &str) -> Option<&'a Vec<Value>> {
    v.get(k).and_then(|x| x.as_array())
}
fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

/// `A.requestKind`: wait / teamPreview / switch / move.
pub fn request_kind(req: Option<&Value>) -> &'static str {
    match req {
        None => "wait",
        Some(r) => {
            if r.get("wait").and_then(|v| v.as_bool()).unwrap_or(false) {
                "wait"
            } else if r.get("teamPreview").and_then(|v| v.as_bool()).unwrap_or(false) {
                "teamPreview"
            } else if r.get("forceSwitch").is_some() {
                "switch"
            } else if r.get("active").is_some() {
                "move"
            } else {
                panic!("Unrecognized request shape")
            }
        }
    }
}

const TARGETS: [&str; 5] = ["none", "foe1", "foe2", "ally", "self"];

fn targets_for(target: Option<&str>) -> Vec<u8> {
    match target {
        Some("normal") | Some("any") => vec![1, 2, 3],
        Some("adjacentFoe") => vec![1, 2],
        Some("adjacentAlly") => vec![3],
        Some("adjacentAllyOrSelf") => vec![3, 4],
        _ => vec![0],
    }
}

fn move_action(m: usize, t: usize, mega: usize) -> usize {
    m * 10 + t * 2 + mega
}

// ---- encode ------------------------------------------------------------------------------

pub fn encode(t: &Tracker) -> Obs {
    let mut cats = vec![0i32; T * C];
    let mut floats = vec![0f32; T * F];
    let req = t.request.as_ref();
    let me = if t.me == "p1" { 0 } else { 1 };
    let foe = 1 - me;
    let kind = request_kind(req);
    let side = [&t.sides[me], &t.sides[foe]];

    let set_cats = |cats: &mut Vec<i32>, tok: usize, values: &[i32]| {
        for (i, v) in values.iter().enumerate() {
            cats[tok * C + i] = *v;
        }
    };
    let set_floats = |floats: &mut Vec<f32>, tok: usize, values: &[f64]| {
        for (i, v) in values.iter().enumerate() {
            floats[tok * F + i] = *v as f32;
        }
    };

    // Pokemon tokens.
    for (s, sd) in side.iter().enumerate() {
        for i in 0..6 {
            let tok = s * 6 + i;
            let Some(mon) = sd.team.get(i) else {
                set_cats(&mut cats, tok, &[id("token", Some(if s == 1 { "foemon" } else { "ownmon" }))]);
                continue;
            };
            let forme = mon.forme.as_deref().and_then(species_by_name);
            let types: Vec<String> = mon.types.clone().unwrap_or_else(|| {
                forme.map(|f| f.types.iter().map(|ty| ty.name().to_string()).collect()).unwrap_or_default()
            });
            let vols: Vec<(&String, &Value)> = mon.volatiles.iter().collect();
            let vol_tail: Vec<&(&String, &Value)> = vols[vols.len().saturating_sub(4)..].iter().collect();
            set_cats(&mut cats, tok, &[
                id("token", Some(if s == 1 { "foemon" } else { "ownmon" })),
                id("species", species_id_of(mon.forme.as_deref().unwrap_or("")).as_deref()),
                id("item", mon.item.as_deref()),
                id("ability", mon.ability.as_deref()),
                id("nature", mon.nature.as_deref()),
                id("move", mon.moves.get(0).map(|x| x.as_str())),
                id("move", mon.moves.get(1).map(|x| x.as_str())),
                id("move", mon.moves.get(2).map(|x| x.as_str())),
                id("move", mon.moves.get(3).map(|x| x.as_str())),
                id("type", types.get(0).map(|x| to_id(x)).as_deref()),
                id("type", types.get(1).map(|x| to_id(x)).as_deref()),
                id("status", Some(mon.status.as_str()).filter(|x| !x.is_empty())),
                id("effect", vol_tail.get(0).map(|(k, _)| k.as_str())),
                id("effect", vol_tail.get(1).map(|(k, _)| k.as_str())),
                id("effect", vol_tail.get(2).map(|(k, _)| k.as_str())),
                id("effect", vol_tail.get(3).map(|(k, _)| k.as_str())),
                id("lastmove", mon.last_move.as_deref()),
            ]);
            let active = if s == 0 {
                req.and_then(|r| as_arr(r, "active")).and_then(|a| mon.slot.and_then(|sl| a.get(sl as usize)))
            } else {
                None
            };
            let mega_stone = mon.item.as_deref().filter(|x| !x.is_empty()).and_then(|it| dex().item_id(it)).map(|i| dex().item(i).is_mega_stone).unwrap_or(false);
            let base = forme.map(|f| f.base);
            let on_field = mon.slot.is_some() && !mon.fainted;
            let exact = if s == 0 { mon.exact } else { None };
            let mut fv: Vec<f64> = Vec::with_capacity(F);
            fv.push(1.0);
            fv.push(mon.hp);
            fv.push(if mon.fainted { 1.0 } else { 0.0 });
            fv.push(if mon.slot.is_some() { 1.0 } else { 0.0 });
            fv.push(if mon.slot == Some(0) { 1.0 } else { 0.0 });
            fv.push(if mon.slot == Some(1) { 1.0 } else { 0.0 });
            fv.push(if s == 1 { if mon.seen { 1.0 } else { 0.0 } } else if mon.brought == Some(true) { 1.0 } else { 0.0 });
            fv.push(if s == 1 { if mon.seen { 0.0 } else { 1.0 } } else if mon.brought.is_none() { 1.0 } else { 0.0 });
            fv.push(if mon.me_evolved { 1.0 } else { 0.0 });
            fv.push(if s == 1 {
                if mega_stone && !sd.mega_used && !mon.me_evolved { 1.0 } else { 0.0 }
            } else {
                if active.and_then(|a| a.get("canMegaEvo")).and_then(|v| v.as_bool()).unwrap_or(false) { 1.0 } else { 0.0 }
            });
            fv.push(if mon.item_gone { 1.0 } else { 0.0 });
            for k in 0..6 {
                fv.push(base.map(|b| b[k] as f64 / 200.0).unwrap_or(0.0));
            }
            let stats = mon.stats.as_ref().and_then(|v| v.as_object());
            for k in ["atk", "def", "spa", "spd", "spe"] {
                fv.push(stats.and_then(|o| o.get(k)).map(|v| num(v) / STAT_SCALE).unwrap_or(0.0));
            }
            fv.push(if mon.stats.is_some() { 1.0 } else { 0.0 });
            for k in BOOSTS {
                fv.push(*mon.boosts.get(k).unwrap_or(&0) as f64 / 6.0);
            }
            for j in 0..4 {
                let v = vol_tail.get(j).map(|(_, v)| *v);
                fv.push(v.map(|v| clamp((t.turn as f64 - v.get("since").and_then(|x| x.as_f64()).unwrap_or(0.0)) / 5.0)).unwrap_or(0.0));
            }
            for j in 0..4 {
                let v = vol_tail.get(j).map(|(_, v)| *v);
                fv.push(v.map(|v| {
                    if let Some(c) = v.get("count").and_then(|x| x.as_f64()) {
                        c / 3.0
                    } else {
                        v.get("elapsed").and_then(|x| x.as_f64()).unwrap_or(0.0) / 4.0
                    }
                }).unwrap_or(0.0));
            }
            fv.push(if exact.is_some() { 1.0 } else { 0.0 });
            fv.push(if mon.commanding.unwrap_or(false) { 1.0 } else { 0.0 });
            fv.push(if mon.types.is_some() { 1.0 } else { 0.0 });
            fv.push(if sd.sheet == Some(true) { 1.0 } else { 0.0 });
            fv.push(exact.map(|e| e[1] / STAT_SCALE).unwrap_or(0.0));
            fv.push(exact.map(|e| e[0] / STAT_SCALE).unwrap_or(0.0));
            fv.push(if mon.status == "slp" { mon.sleep_elapsed / 3.0 } else { 0.0 });
            fv.push(if mon.status == "slp" && mon.sleep_rest { 1.0 } else { 0.0 });
            fv.push(if mon.status == "tox" && on_field { mon.tox_stage / 8.0 } else { 0.0 });
            fv.push(if on_field { clamp(t.turns_since_entry(mon) / 5.0) } else { 0.0 });
            fv.push(if on_field && mon.moves_since_entry == 0.0 { 1.0 } else { 0.0 });
            fv.push(if on_field { clamp(mon.moves_since_entry / 3.0) } else { 0.0 });
            fv.push(if on_field { t.stall_streak(mon) as f64 / 3.0 } else { 0.0 });
            fv.push(if mon.last_move.is_some() { 1.0 } else { 0.0 });
            fv.push(mon.last_move.as_ref().map(|_| clamp((t.turn as f64 - mon.last_move_turn.unwrap_or(0.0)) / 5.0)).unwrap_or(0.0));
            set_floats(&mut floats, tok, &fv);
        }
    }

    // Active slot tokens.
    let turns = recent_turns(t);
    for (k, (s, slot)) in [(0usize, 0usize), (0, 1), (1, 0), (1, 1)].iter().enumerate() {
        let tok = 12 + k;
        let sd = side[*s];
        let index = sd.active[*slot];
        let acts: Vec<Option<&crate::tracker::Action>> = turns.iter().map(|h| {
            h.actions.iter().find(|a| a.side == sd.id && a.slot == Some(*slot as i32) && a.phase == "turn")
        }).collect();
        let movers: Vec<usize> = turns.iter().map(|h| {
            let c = h.actions.iter().filter(|a| a.kind != "switch" || a.phase == "turn").count();
            if c == 0 { 1 } else { c }
        }).collect();
        let occupant = index.and_then(|i| sd.team.get(i as usize));
        let act = |j: usize| acts.get(j).copied().flatten();
        set_cats(&mut cats, tok, &[
            id("token", Some(if *s == 0 { "ownactive" } else { "foeactive" })),
            index.map(|i| id("pointer", Some(&pointer_name(sd, i, side[0].id.as_str())))).unwrap_or(vocab().fields["pointer"].offset),
            id("move1", act(0).and_then(|a| if a.kind == "move" { a.id.as_deref() } else { None })),
            id("move2", act(1).and_then(|a| if a.kind == "move" { a.id.as_deref() } else { None })),
            id("action1", act(0).map(|a| a.kind.as_str())),
            id("action2", act(1).map(|a| a.kind.as_str())),
            id("target1", act(0).and_then(|a| relative_target(a, sd.id.as_str(), *slot))),
            id("target2", act(1).and_then(|a| relative_target(a, sd.id.as_str(), *slot))),
            id("actor1", act(0).map(|a| pointer_name(sd, a.actor, side[0].id.as_str())).as_deref()),
            id("actor2", act(1).map(|a| pointer_name(sd, a.actor, side[0].id.as_str())).as_deref()),
        ]);
        let fs: Vec<f64> = vec![
            if index.is_none() { 0.0 } else { 1.0 },
            act(0).map(|a| a.order as f64 / movers[0] as f64).unwrap_or(0.0),
            act(1).map(|a| a.order as f64 / movers.get(1).copied().unwrap_or(1) as f64).unwrap_or(0.0),
            if act(0).is_some() { 1.0 } else { 0.0 },
            if act(1).is_some() { 1.0 } else { 0.0 },
            if act(0).map(|a| a.missed).unwrap_or(false) { 1.0 } else { 0.0 },
            if act(0).map(|a| a.spread).unwrap_or(false) { 1.0 } else { 0.0 },
            if act(0).map(|a| Some(a.actor) == index).unwrap_or(false) { 1.0 } else { 0.0 },
            if act(1).map(|a| Some(a.actor) == index).unwrap_or(false) { 1.0 } else { 0.0 },
            if occupant.map(|o| !o.fainted && o.moves_since_entry == 0.0).unwrap_or(false) { 1.0 } else { 0.0 },
            occupant.filter(|o| !o.fainted).map(|o| clamp(t.turns_since_entry(o) / 5.0)).unwrap_or(0.0),
        ];
        set_floats(&mut floats, tok, &fs);
    }

    // Side tokens.
    for (s, sd) in side.iter().enumerate() {
        let tok = 16 + s;
        let conds: Vec<(&String, &Value)> = sd.conditions.iter().collect();
        let cond_tail: Vec<&(&String, &Value)> = conds[conds.len().saturating_sub(8)..].iter().collect();
        let team: Vec<&Mon> = sd.team.iter().collect();
        set_cats(&mut cats, tok, &[
            id("token", Some(if s == 1 { "foeside" } else { "ownside" })),
            id("effect", cond_tail.get(0).map(|(k, _)| k.as_str())),
            id("effect", cond_tail.get(1).map(|(k, _)| k.as_str())),
            id("effect", cond_tail.get(2).map(|(k, _)| k.as_str())),
            id("effect", cond_tail.get(3).map(|(k, _)| k.as_str())),
            id("effect", cond_tail.get(4).map(|(k, _)| k.as_str())),
            id("effect", cond_tail.get(5).map(|(k, _)| k.as_str())),
            id("effect", cond_tail.get(6).map(|(k, _)| k.as_str())),
            id("effect", cond_tail.get(7).map(|(k, _)| k.as_str())),
        ]);
        let mut fs: Vec<f64> = Vec::new();
        fs.push(if sd.mega_used { 1.0 } else { 0.0 });
        fs.push(sd.team_size.unwrap_or(0.0) / 6.0);
        let brought = |m: &Mon| if s == 1 { m.seen } else { m.brought == Some(true) };
        fs.push(team.iter().filter(|m| brought(m) && !m.fainted).count() as f64 / 4.0);
        fs.push(team.iter().filter(|m| m.fainted).count() as f64 / 4.0);
        fs.push(team.iter().filter(|m| m.seen).count() as f64 / 4.0);
        for j in 0..8 {
            let v = cond_tail.get(j).map(|(_, v)| *v);
            fs.push(v.map(|v| clamp((t.turn as f64 - v.get("since").and_then(|x| x.as_f64()).unwrap_or(0.0)) / 8.0)).unwrap_or(0.0));
        }
        for j in 0..8 {
            let v = cond_tail.get(j).map(|(_, v)| *v);
            fs.push(v.map(|v| v.get("layers").and_then(|x| x.as_f64()).unwrap_or(0.0) / 3.0).unwrap_or(0.0));
        }
        fs.push(if sd.sheet == Some(true) { 1.0 } else { 0.0 });
        set_floats(&mut floats, tok, &fs);
    }

    // Field token.
    {
        let pseudo: Vec<(&String, &Value)> = t.field.pseudo.iter().collect();
        let ptail: Vec<&(&String, &Value)> = pseudo[pseudo.len().saturating_sub(4)..].iter().collect();
        let w = t.field.weather.as_ref();
        let terrain = t.field.terrain.as_ref();
        set_cats(&mut cats, 18, &[
            id("token", Some("field")),
            id("effect", w.and_then(|w| w.get("id")).and_then(|v| v.as_str())),
            id("effect", terrain.and_then(|w| w.get("id")).and_then(|v| v.as_str())),
            id("effect", ptail.get(0).map(|(k, _)| k.as_str())),
            id("effect", ptail.get(1).map(|(k, _)| k.as_str())),
            id("effect", ptail.get(2).map(|(k, _)| k.as_str())),
            id("effect", ptail.get(3).map(|(k, _)| k.as_str())),
            id("request", Some(if kind == "teamPreview" { "teampreview" } else { kind })),
        ]);
        let active = req.and_then(|r| as_arr(r, "active"));
        let force = req.and_then(|r| as_arr(r, "forceSwitch"));
        let mut fs: Vec<f64> = Vec::new();
        fs.push(w.map(|w| clamp((t.turn as f64 - w.get("since").and_then(|x| x.as_f64()).unwrap_or(0.0)) / 8.0)).unwrap_or(0.0));
        fs.push(terrain.map(|w| clamp((t.turn as f64 - w.get("since").and_then(|x| x.as_f64()).unwrap_or(0.0)) / 8.0)).unwrap_or(0.0));
        for j in 0..4 {
            let v = ptail.get(j).map(|(_, v)| *v);
            fs.push(v.map(|v| clamp((t.turn as f64 - v.get("since").and_then(|x| x.as_f64()).unwrap_or(0.0)) / 5.0)).unwrap_or(0.0));
        }
        fs.push(clamp(t.turn as f64 / 30.0));
        for slot in 0..2 {
            fs.push(if force.and_then(|a| a.get(slot)).and_then(|v| v.as_bool()).unwrap_or(false) { 1.0 } else { 0.0 });
        }
        for slot in 0..2 {
            fs.push(if active.and_then(|a| a.get(slot)).and_then(|v| v.get("trapped")).and_then(|v| v.as_bool()).unwrap_or(false) { 1.0 } else { 0.0 });
        }
        fs.push(if kind == "teamPreview" { 1.0 } else { 0.0 });
        for _ in 0..6 {
            fs.push(0.0);
        }
        set_floats(&mut floats, 18, &fs);
    }

    // Own move tokens.
    for slot in 0..2 {
        for j in 0..4 {
            let tok = 19 + slot * 4 + j;
            let mv = if kind == "move" {
                req.and_then(|r| as_arr(r, "active")).and_then(|a| a.get(slot)).and_then(|a| as_arr(a, "moves")).and_then(|m| m.get(j))
            } else {
                None
            };
            let Some(mv) = mv else {
                set_cats(&mut cats, tok, &[
                    id("token", Some("ownmove")),
                    id("moveslot", Some(&(j + 1).to_string())),
                    id("activeslot", Some(&"ab".chars().nth(slot).unwrap().to_string())),
                ]);
                continue;
            };
            let mid = as_str(mv, "id").unwrap_or("");
            let info = move_info(&to_id(mid));
            let target = as_str(mv, "target");
            set_cats(&mut cats, tok, &[
                id("token", Some("ownmove")),
                id("move", Some(&to_id(mid))),
                id("type", info.map(|d| to_id(d.ty)).as_deref()),
                id("target", target),
                id("category", info.map(|d| to_id(category_name(d.category))).as_deref()),
                id("moveslot", Some(&(j + 1).to_string())),
                id("activeslot", Some(&"ab".chars().nth(slot).unwrap().to_string())),
            ]);
            let pp = mv.get("pp").and_then(|v| v.as_f64());
            let maxpp = mv.get("maxpp").and_then(|v| v.as_f64());
            let disabled = mv.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut fs: Vec<f64> = Vec::new();
            fs.push(1.0);
            fs.push(match (pp, maxpp) { (Some(p), Some(m)) if m != 0.0 => p / m, _ => 1.0 });
            fs.push(if disabled { 1.0 } else { 0.0 });
            fs.push(info.map(|d| d.base_power as f64 / 150.0).unwrap_or(0.0));
            fs.push(info.map(|d| if d.accuracy == 0 { 1.0 } else { d.accuracy as f64 / 100.0 }).unwrap_or(1.0));
            fs.push(info.map(|d| d.priority as f64 / 5.0).unwrap_or(0.0));
            fs.push(if targets_for(target).contains(&3) { 1.0 } else { 0.0 });
            fs.push(if matches!(target, Some("allAdjacent") | Some("allAdjacentFoes")) { 1.0 } else { 0.0 });
            fs.push(if target.is_none() { 1.0 } else { 0.0 });
            fs.push(info.map(|d| if d.contact { 1.0 } else { 0.0 }).unwrap_or(0.0));
            fs.push(info.map(|d| if d.category == Category::Status { 1.0 } else { 0.0 }).unwrap_or(0.0));
            set_floats(&mut floats, tok, &fs);
        }
    }

    // Request side.pokemon position -> own token index (switch actions and preview plans).
    let mut switch_map = [-1i8; 6];
    if let Some(req) = req {
        if req.get("wait").and_then(|v| v.as_bool()) != Some(true) {
            if let Some(pokemon) = req.get("side").and_then(|s| get(s, "pokemon")).and_then(|v| v.as_array()) {
                for (position, entry) in pokemon.iter().enumerate() {
                    let ident = as_str(entry, "ident").unwrap_or("");
                    let name = ident.get(4..).unwrap_or("");
                    if let Some(index) = t.sides[me].names.get(name) {
                        if position < 6 {
                            switch_map[position] = *index as i8;
                        }
                    }
                }
            }
        }
    }

    Obs { cats, floats, switch_map, kind: kind.to_string() }
}

/// A move's encoder-visible data. `struggle` is special: Showdown has it as a Normal/Physical/50
/// move, while the engine models it as a typeless pseudo-move, so the encoder uses Showdown's values.
#[derive(Clone, Copy)]
struct MvInfo {
    ty: &'static str,
    category: Category,
    base_power: u16,
    accuracy: u8,
    priority: i8,
    contact: bool,
}

fn move_info(mid: &str) -> Option<MvInfo> {
    if mid == "struggle" {
        return Some(MvInfo { ty: "Normal", category: Category::Physical, base_power: 50, accuracy: 0, priority: 0, contact: true });
    }
    dex().move_id(mid).map(|i| {
        let d = dex().mv(i);
        MvInfo { ty: d.ty.name(), category: d.category, base_power: d.base_power, accuracy: d.accuracy, priority: d.priority, contact: d.flags & crate::dex::flag::CONTACT != 0 }
    })
}

fn category_name(c: Category) -> &'static str {
    match c {
        Category::Physical => "physical",
        Category::Special => "special",
        Category::Status => "status",
    }
}

fn pointer_name(side: &crate::tracker::Side, index: i32, own: &str) -> String {
    let base = if side.id == own { 0 } else { 6 };
    format!("mon{}", base + index)
}

fn relative_target(a: &crate::tracker::Action, side_id: &str, slot: usize) -> Option<&'static str> {
    if a.spread {
        return Some("spread");
    }
    let (t_side, t_slot) = a.target.as_ref()?;
    if t_side != side_id {
        return Some(if *t_slot == 0 { "foe-a" } else { "foe-b" });
    }
    Some(if *t_slot as usize == slot { "self" } else { "ally" })
}

fn recent_turns(t: &Tracker) -> Vec<&crate::tracker::Hist> {
    let with: Vec<&crate::tracker::Hist> = t.history().iter().filter(|h| !h.actions.is_empty()).collect();
    with.iter().rev().take(2).copied().collect()
}

// ---- masks -------------------------------------------------------------------------------

fn preview_plans() -> &'static Vec<[usize; 4]> {
    static PLANS: OnceLock<Vec<[usize; 4]>> = OnceLock::new();
    PLANS.get_or_init(|| {
        let mut out = Vec::new();
        for a in 0..6 {
            for b in (a + 1)..6 {
                let rest: Vec<usize> = (0..6).filter(|i| *i != a && *i != b).collect();
                for c in 0..4 {
                    for d in (c + 1)..4 {
                        out.push([a, b, rest[c], rest[d]]);
                    }
                }
            }
        }
        out
    })
}

fn fainted(mon: &Value) -> bool {
    as_str(mon, "condition").map(|c| c.ends_with(" fnt")).unwrap_or(false)
}

fn preview_mask(req: &Value) -> Vec<u8> {
    let n = req.get("side").and_then(|s| get(s, "pokemon")).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    preview_plans().iter().map(|p| if p.iter().all(|i| *i < n) { 1 } else { 0 }).collect()
}

/// The requested joint action for `slot` given the earlier slot's action (`None` for slot 0).
fn slot_mask(req: &Value, slot: usize, prior: Option<usize>) -> Vec<u8> {
    let mut mask = vec![0u8; SLOT_ACTIONS];
    let kind = request_kind(Some(req));
    let pokemon = req.get("side").and_then(|s| get(s, "pokemon")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let force = req.get("forceSwitch").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let used_switch = prior.map(|a| describe(a)).map(|d| if d.0 == "switch" { d.1 as i64 } else { -1 }).unwrap_or(-1);
    if kind == "switch" {
        if force.get(slot).and_then(|v| v.as_bool()) != Some(true) {
            mask[PASS] = 1;
            return mask;
        }
        let (mut switches, mut passes) = switch_budget(req);
        if slot == 1 && force.get(0).and_then(|v| v.as_bool()) == Some(true) {
            if let Some(a) = prior {
                if describe(a).0 == "pass" { passes -= 1 } else { switches -= 1 }
            }
        }
        if pokemon.get(slot).and_then(|m| m.get("reviving")).and_then(|v| v.as_bool()) == Some(true) {
            for (i, mon) in pokemon.iter().enumerate() {
                if fainted(mon) {
                    mask[SWITCH_BASE + i] = 1;
                }
            }
            return mask;
        }
        for i in force.len()..pokemon.len() {
            if !fainted(&pokemon[i]) && i as i64 != used_switch {
                mask[SWITCH_BASE + i] = 1;
            }
        }
        if passes > 0 {
            mask[PASS] = 1;
        }
        return mask;
    }
    if kind != "move" {
        panic!("slotMask needs a move or switch request");
    }
    let mon = &pokemon[slot];
    if fainted(mon) || mon.get("commanding").and_then(|v| v.as_bool()).unwrap_or(false) {
        mask[PASS] = 1;
        return mask;
    }
    let active = &req["active"][slot];
    let mega_used = prior.map(|a| describe(a)).map(|d| d.0 == "move" && d.2 == 1).unwrap_or(false);
    let can_mega = active.get("canMegaEvo").and_then(|v| v.as_bool()).unwrap_or(false);
    if let Some(moves) = as_arr(active, "moves") {
        for (m, mv) in moves.iter().enumerate() {
            if mv.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false) {
                continue;
            }
            for t in targets_for(as_str(mv, "target")) {
                mask[move_action(m, t as usize, 0)] = 1;
                if can_mega && !mega_used {
                    mask[move_action(m, t as usize, 1)] = 1;
                }
            }
        }
    }
    if !active.get("trapped").and_then(|v| v.as_bool()).unwrap_or(false) {
        for i in req["active"].as_array().map(|a| a.len()).unwrap_or(0)..pokemon.len() {
            if !fainted(&pokemon[i]) && i as i64 != used_switch {
                mask[SWITCH_BASE + i] = 1;
            }
        }
    }
    mask
}

fn describe(a: usize) -> (&'static str, usize, usize) {
    if a == PASS {
        ("pass", 0, 0)
    } else if a >= SWITCH_BASE {
        ("switch", a - SWITCH_BASE, 0)
    } else {
        ("move", a / 10, a % 2)
    }
}

fn switch_budget(req: &Value) -> (i64, i64) {
    let pokemon = req.get("side").and_then(|s| get(s, "pokemon")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let force = req.get("forceSwitch").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let out = force.iter().filter(|v| v.as_bool() == Some(true)).count() as i64;
    let inn = pokemon.iter().skip(force.len()).filter(|m| !fainted(m)).count() as i64;
    let switches = out.min(inn);
    (switches, out - switches)
}

pub fn masks(req: Option<&Value>) -> Masks {
    let kind = request_kind(req);
    match (kind, req) {
        ("teamPreview", Some(r)) => Masks { kind: kind.into(), preview: Some(preview_mask(r)), mask0: None, mask1: None },
        ("wait", _) | (_, None) => Masks { kind: kind.into(), preview: None, mask0: None, mask1: None },
        (_, Some(r)) => {
            let mask0 = slot_mask(r, 0, None);
            let mut mask1 = vec![0u8; SLOT_ACTIONS * SLOT_ACTIONS];
            for a0 in 0..SLOT_ACTIONS {
                if mask0[a0] == 1 {
                    let m = slot_mask(r, 1, Some(a0));
                    mask1[a0 * SLOT_ACTIONS..(a0 + 1) * SLOT_ACTIONS].copy_from_slice(&m);
                }
            }
            Masks { kind: kind.into(), preview: None, mask0: Some(mask0), mask1: Some(mask1) }
        }
    }
}
