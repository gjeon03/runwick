//! Power source. macOS reports it through `pmset -g batt`; elsewhere (or when that fails)
//! the source is unknown, which automation treats like AC and the UI shows as "확인 불가".

use runwick_core::api::jobs::PowerSource;
use std::process::{Command, Stdio};

pub fn parse_pmset(out: &str) -> PowerSource {
    if out.contains("'AC Power'") {
        PowerSource::Ac
    } else if out.contains("'Battery Power'") {
        PowerSource::Battery
    } else {
        PowerSource::Unknown
    }
}

/// Read the current source (spawns `pmset` on macOS; cheap, called at most once a minute).
pub fn read() -> PowerSource {
    if !cfg!(target_os = "macos") {
        return PowerSource::Unknown;
    }
    match Command::new("/usr/bin/pmset").args(["-g", "batt"]).stdin(Stdio::null()).stderr(Stdio::null()).output() {
        Ok(out) if out.status.success() => parse_pmset(&String::from_utf8_lossy(&out.stdout)),
        _ => PowerSource::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pmset_output() {
        let ac = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t70%; charging; present: true\n";
        let batt = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t64%; discharging;\n";
        assert_eq!(parse_pmset(ac), PowerSource::Ac);
        assert_eq!(parse_pmset(batt), PowerSource::Battery);
        assert_eq!(parse_pmset("No batteries\n"), PowerSource::Unknown);
    }
}
