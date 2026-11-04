//! Native tracker parity. Replays the fixture `fixtures/tracker.jsonl.gz` (produced by
//! `adapters/vgczero/parity/track.js`) into the native `Tracker` and requires its canonical state
//! to equal the JS tracker's after every decision step. Numbers compare by value (JS `1` == Rust
//! `1.0`); object keys and array lengths must match exactly.
use std::io::Read;

use serde_json::Value;
use vgczero_engine::tracker::Tracker;
use vgczero_engine::{data_path, load_dex};

fn load() -> Option<Vec<Value>> {
    load_dex(&data_path("dex.json.gz")).unwrap();
    let path = data_path("fixtures/tracker.jsonl.gz");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => {
            eprintln!("skipping: no fixture {path} (generate it with `node track.js`)");
            return None;
        }
    };
    let mut text = String::new();
    flate2::read::GzDecoder::new(&bytes[..]).read_to_string(&mut text).unwrap();
    Some(text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect())
}

fn eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) { (Some(a), Some(b)) => (a - b).abs() < 1e-9, _ => false },
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).map(|w| eq(v, w)).unwrap_or(false))
        }
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(v, w)| eq(v, w)),
        _ => a == b,
    }
}

fn diff(a: &Value, b: &Value, path: &str) -> Option<String> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            match (x.as_f64(), y.as_f64()) { (Some(a), Some(b)) if (a - b).abs() >= 1e-9 => Some(format!("{path}: {x} vs {y}")), _ => None }
        }
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys() {
                match y.get(k) {
                    Some(w) => if let Some(d) = diff(&x[k], w, &format!("{path}.{k}")) { return Some(d); },
                    None => return Some(format!("{path}.{k}: present in rust only")),
                }
            }
            for k in y.keys() {
                if !x.contains_key(k) { return Some(format!("{path}.{k}: present in js only")); }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() { return Some(format!("{path}: array len {} vs {}", x.len(), y.len())); }
            for (i, (v, w)) in x.iter().zip(y).enumerate() {
                if let Some(d) = diff(v, w, &format!("{path}[{i}]")) { return Some(d); }
            }
            None
        }
        _ if a == b => None,
        _ => Some(format!("{path}: {} vs {}", a, b)),
    }
}

#[test]
fn tracker_state_matches_js() {
    let Some(games) = load() else { return };
    let (mut checked, mut failures) = (0usize, Vec::new());
    for (gi, g) in games.iter().enumerate() {
        let mut trackers = [Tracker::new(0, false), Tracker::new(1, false)];
        for (si, step) in g["steps"].as_array().unwrap().iter().enumerate() {
            let id = step["id"].as_str().unwrap();
            let s = if id == "p1" { 0 } else { 1 };
            let lines: Vec<String> = step["lines"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
            let tr = &mut trackers[s];
            tr.feed(&lines);
            tr.observe_request(&step["request"]);
            let got = tr.to_value();
            let want: Value = serde_json::from_str(step["state"].as_str().unwrap()).unwrap();
            checked += 1;
            if !eq(&got, &want) {
                let where_ = diff(&got, &want, "").unwrap_or_else(|| "(no path)".into());
                if failures.len() < 8 {
                    failures.push(format!("game {gi} step {si} {id}: {where_}"));
                }
            }
        }
    }
    eprintln!("tracker parity: checked {checked} steps, {} failing", failures.len());
    assert!(failures.is_empty(), "first mismatches:\n{}", failures.join("\n"));
}
