//! Offline privileged oracle probe, not a battle API or network protocol.
use quark_drive_core::rng::SodiumRng;
use std::io::{self, BufRead, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut output = io::BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let line = line?;
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 3 { return Err("expected seed/from/to".into()); }
        let mut rng = SodiumRng::from_hex(fields[0])?;
        let mut raw = rng;
        let optional = |text: &str| -> Result<Option<i64>, std::num::ParseIntError> {
            if text == "-" { Ok(None) } else { text.parse().map(Some) }
        };
        let value = rng.random(optional(fields[1])?, optional(fields[2])?);
        writeln!(output, "{}\t{}\t{}", raw.next_u32(), value, rng.seed_hex())?;
    }
    Ok(())
}
