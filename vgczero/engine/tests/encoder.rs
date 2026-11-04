//! Native v3 encoder parity. Replays the same `track.js` fixture as `tests/tracker.rs`, then
//! requires the native `encode`/`masks` output to equal the JS `encoder.cjs`/`actions.cjs` output
//! recorded per step. `cats` and the masks compare exactly; `floats` allow a 1e-5 tolerance
//! (identical f64 math cast to f32 still rounds the same, but guard against reordering).
use std::io::Read;

use serde_json::Value;
use vgczero_engine::encoder::{encode, masks, C, F, T};
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

fn nums(v: &Value) -> Vec<f64> {
    v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect()
}

fn check_cats(got: &[i32], want: &Value, path: &str, bad: &mut Vec<String>) {
    let w = nums(want);
    if got.len() != w.len() {
        bad.push(format!("{path}.cats len {} vs {}", got.len(), w.len()));
        return;
    }
    for (i, (g, x)) in got.iter().zip(w.iter()).enumerate() {
        if *g as f64 != *x {
            bad.push(format!("{path}.cats[{i}]: {g} vs {x}"));
            if bad.len() > 6 {
                return;
            }
        }
    }
}

fn check_floats(got: &[f32], want: &Value, path: &str, bad: &mut Vec<String>) {
    let w = nums(want);
    if got.len() != w.len() {
        bad.push(format!("{path}.floats len {} vs {}", got.len(), w.len()));
        return;
    }
    for (i, (g, x)) in got.iter().zip(w.iter()).enumerate() {
        if (*g as f64 - *x).abs() > 1e-5 {
            bad.push(format!("{path}.floats[{i}]: {g} vs {x}"));
            if bad.len() > 6 {
                return;
            }
        }
    }
}

fn check_mask(got: &Option<Vec<u8>>, want: &Value, path: &str, bad: &mut Vec<String>) {
    match want {
        Value::Null => {
            if got.is_some() {
                bad.push(format!("{path}: got a mask, JS had none"));
            }
        }
        _ => {
            let w = nums(want);
            let Some(got) = got else {
                bad.push(format!("{path}: missing mask"));
                return;
            };
            if got.len() != w.len() {
                bad.push(format!("{path} len {} vs {}", got.len(), w.len()));
                return;
            }
            for (i, (g, x)) in got.iter().zip(w.iter()).enumerate() {
                if *g as f64 != *x {
                    bad.push(format!("{path}[{i}]: {g} vs {x}"));
                    if bad.len() > 6 {
                        return;
                    }
                }
            }
        }
    }
}

#[test]
fn encoder_matches_js() {
    let Some(games) = load() else { return };
    let (mut checked, mut bad) = (0usize, Vec::new());
    for (gi, g) in games.iter().enumerate() {
        let mut trackers = [Tracker::new(0, false), Tracker::new(1, false)];
        for (si, step) in g["steps"].as_array().unwrap().iter().enumerate() {
            let id = step["id"].as_str().unwrap();
            let s = if id == "p1" { 0 } else { 1 };
            let lines: Vec<String> = step["lines"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
            let tr = &mut trackers[s];
            tr.feed(&lines);
            tr.observe_request(&step["request"]);
            checked += 1;
            let tag = format!("game {gi} step {si} {id}");
            let obs = encode(tr);
            assert_eq!(obs.cats.len(), T * C);
            assert_eq!(obs.floats.len(), T * F);
            check_cats(&obs.cats, &step["obs"]["cats"], &tag, &mut bad);
            check_floats(&obs.floats, &step["obs"]["floats"], &tag, &mut bad);
            // switchMap
            let want_sm = nums(&step["obs"]["switchMap"]);
            for (i, (g, x)) in obs.switch_map.iter().zip(want_sm.iter()).enumerate() {
                if *g as f64 != *x {
                    bad.push(format!("{tag}.switchMap[{i}]: {g} vs {x}"));
                }
            }
            if obs.kind != step["obs"]["kind"].as_str().unwrap() {
                bad.push(format!("{tag}.kind: {} vs {}", obs.kind, step["obs"]["kind"]));
            }
            let m = masks(tr.request());
            if m.kind != step["masks"]["kind"].as_str().unwrap() {
                bad.push(format!("{tag}.masks.kind: {} vs {}", m.kind, step["masks"]["kind"]));
            }
            check_mask(&m.preview, &step["masks"]["preview"], &format!("{tag}.preview"), &mut bad);
            check_mask(&m.mask0, &step["masks"]["mask0"], &format!("{tag}.mask0"), &mut bad);
            check_mask(&m.mask1, &step["masks"]["mask1"], &format!("{tag}.mask1"), &mut bad);
            if bad.len() > 6 {
                break;
            }
        }
        if bad.len() > 6 {
            break;
        }
    }
    eprintln!("encoder parity: checked {checked} steps, {} issues", bad.len());
    assert!(bad.is_empty(), "first mismatches:\n{}", bad.join("\n"));
}
