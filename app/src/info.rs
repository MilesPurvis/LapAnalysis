//! What you record about a session after downloading it: a name, who drove what where,
//! tyres and pressures, and notes. Stored per file in the library's index.json.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

const SHORT_MAX: usize = 120;
const NOTES_MAX: usize = 20_000;
/// Generous upper bound for a tyre pressure in either unit (race tyres run ~20–40 psi).
const PRESSURE_MAX: f64 = 100.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionInfo {
    /// Your name for the session, e.g. "Practice 2".
    pub name: String,
    pub driver: String,
    pub vehicle: String,
    /// Overrides the logger's track name, which is often truncated ("Great Bend T").
    pub track: String,
    pub tyres: Tyres,
    pub notes: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tyres {
    pub compound: String,
    /// `psi` or `bar`; empty means psi.
    pub unit: String,
    pub cold: Corners,
    pub hot: Corners,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Corners {
    pub fl: Option<f64>,
    pub fr: Option<f64>,
    pub rl: Option<f64>,
    pub rr: Option<f64>,
}

impl SessionInfo {
    /// Trim whitespace and reject values that can't be right, before saving.
    pub fn cleaned(mut self) -> Result<Self> {
        for (label, value) in [
            ("session name", &mut self.name),
            ("driver", &mut self.driver),
            ("car", &mut self.vehicle),
            ("track", &mut self.track),
            ("tyre compound", &mut self.tyres.compound),
        ] {
            *value = value.split_whitespace().collect::<Vec<_>>().join(" ");
            if value.chars().count() > SHORT_MAX {
                bail!("{label} is longer than {SHORT_MAX} characters");
            }
        }
        self.notes = self.notes.trim().replace("\r\n", "\n");
        if self.notes.chars().count() > NOTES_MAX {
            bail!("notes are longer than {NOTES_MAX} characters");
        }
        self.tyres.unit = self.tyres.unit.trim().to_ascii_lowercase();
        if !matches!(self.tyres.unit.as_str(), "" | "psi" | "bar") {
            bail!("pressure unit must be psi or bar");
        }
        for (when, c) in [("cold", &self.tyres.cold), ("hot", &self.tyres.hot)] {
            for (corner, p) in [("FL", c.fl), ("FR", c.fr), ("RL", c.rl), ("RR", c.rr)] {
                if p.is_some_and(|p| !p.is_finite() || p <= 0.0 || p > PRESSURE_MAX) {
                    bail!("{when} {corner} pressure must be between 0 and {PRESSURE_MAX}");
                }
            }
        }
        Ok(self)
    }
}

/// A download name you can recognise, keeping the logger's name for traceability:
/// `2026-09-07 Grand Bend - Practice 2 (a_0124).xrz`. Falls back to the plain file name.
pub fn friendly_file_name(file: &str, date: &str, track: &str, title: &str) -> String {
    let (stem, ext) = file.rsplit_once('.').unwrap_or((file, ""));
    let label = [track, title].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" - ");
    if label.is_empty() {
        return file.to_string();
    }
    let label = format!("{date} {label}").trim().to_string();
    let safe: String = label
        .chars()
        .map(|c| if c.is_alphanumeric() || " -_.()&+,'".contains(c) { c } else { '-' })
        .collect();
    format!("{safe} ({stem}).{ext}")
}
