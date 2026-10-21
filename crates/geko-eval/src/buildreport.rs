//! Comparison of the `build_report.json` of two builds of the same data on the same machine:
//! time and peak memory of each stage that ran in both, and the size of the bundle.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Stage {
    pub stage: String,
    pub elapsed_ms: u64,
    pub peak_mb: Option<u64>,
    pub reused: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Bundle {
    pub bytes: u64,
    pub hot_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BuildReport {
    pub stages: Vec<Stage>,
    #[serde(default)]
    pub bundle: Bundle,
}

/// A stage is slower only past this factor and this many milliseconds: build times vary with
/// the disk cache and whatever else the machine runs.
pub const TIME_FACTOR: f64 = 1.5;
pub const TIME_MARGIN_MS: u64 = 5_000;
/// Peak memory varies far less than time between runs.
pub const MEMORY_FACTOR: f64 = 1.2;
pub const MEMORY_MARGIN_MB: u64 = 200;
/// The bundle grows with fixes too (a missing name now indexed), but not by a tenth.
pub const SIZE_FACTOR: f64 = 1.1;

/// What of the stages to compare. Time is only comparable between builds run in the same
/// moments, since it follows whatever else the machine does; peak memory, between builds on the
/// same machine, since it grows with its cores. The sizes of the bundle are always compared.
#[derive(Debug, Clone, Copy)]
pub struct Compare {
    pub time: bool,
    pub memory: bool,
}

/// Stages slower or hungrier than in `baseline`, as `compare` says (among those that ran in both,
/// not reused), a bundle or a hot part clearly larger, and a hot part above `max_hot_bytes`.
pub fn regressions(baseline: &BuildReport, report: &BuildReport, max_hot_bytes: Option<u64>, compare: Compare) -> Vec<String> {
    let mut out = Vec::new();
    for r in report.stages.iter().filter(|s| !s.reused) {
        let Some(b) = baseline.stages.iter().find(|b| b.stage == r.stage && !b.reused) else { continue };
        if compare.time && r.elapsed_ms as f64 > b.elapsed_ms as f64 * TIME_FACTOR && r.elapsed_ms > b.elapsed_ms + TIME_MARGIN_MS {
            out.push(format!("{}: {:.1} -> {:.1} s", r.stage, b.elapsed_ms as f64 / 1000.0, r.elapsed_ms as f64 / 1000.0));
        }
        if compare.memory
            && let (Some(bm), Some(rm)) = (b.peak_mb, r.peak_mb)
            && rm as f64 > bm as f64 * MEMORY_FACTOR
            && rm > bm + MEMORY_MARGIN_MB
        {
            out.push(format!("{}: peak memory {bm} -> {rm} MB", r.stage));
        }
    }
    let (b, r) = (&baseline.bundle, &report.bundle);
    for (what, before, now) in [("bundle", b.bytes, r.bytes), ("hot part of the bundle", b.hot_bytes, r.hot_bytes)] {
        if before > 0 && now as f64 > before as f64 * SIZE_FACTOR {
            out.push(format!("{what}: {:.0} -> {:.0} MB", before as f64 / 1e6, now as f64 / 1e6));
        }
    }
    if let Some(max) = max_hot_bytes
        && r.hot_bytes > max
    {
        out.push(format!("hot part of the bundle: {:.2} GB, above the objective of {:.2} GB", r.hot_bytes as f64 / 1e9, max as f64 / 1e9));
    }
    out
}

pub fn table(baseline: &BuildReport, report: &BuildReport) -> String {
    let mut out = format!("{:<28} {:>22} {:>22}\n", "stage", "baseline s / peak MB", "now s / peak MB");
    let cell = |s: Option<&Stage>| match s {
        Some(s) if s.reused => "reused".to_owned(),
        Some(s) => format!("{:.1} / {}", s.elapsed_ms as f64 / 1000.0, s.peak_mb.map_or("-".into(), |m| m.to_string())),
        None => "-".to_owned(),
    };
    for r in &report.stages {
        let b = baseline.stages.iter().find(|b| b.stage == r.stage);
        out.push_str(&format!("{:<28} {:>22} {:>22}\n", r.stage, cell(b), cell(Some(r))));
    }
    let mb = |b: u64| format!("{:.0} MB", b as f64 / 1e6);
    out.push_str(&format!("{:<28} {:>22} {:>22}\n", "bundle", mb(baseline.bundle.bytes), mb(report.bundle.bytes)));
    out.push_str(&format!("{:<28} {:>22} {:>22}\n", "hot part", mb(baseline.bundle.hot_bytes), mb(report.bundle.hot_bytes)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(ms: u64, mb: u64, reused: bool, hot: u64) -> BuildReport {
        BuildReport {
            stages: vec![Stage { stage: "index ES".into(), elapsed_ms: ms, peak_mb: Some(mb), reused }],
            bundle: Bundle { bytes: 2_000_000_000, hot_bytes: hot },
        }
    }

    #[test]
    fn slower_hungrier_or_larger_builds_are_regressions() {
        let all = Compare { time: true, memory: true };
        let sizes = Compare { time: false, memory: false };
        let base = report(75_000, 2_900, false, 1_000_000_000);
        assert!(regressions(&base, &report(90_000, 3_000, false, 1_050_000_000), None, all).is_empty(), "noise");
        assert_eq!(regressions(&base, &report(136_000, 2_900, false, 1_000_000_000), None, all), ["index ES: 75.0 -> 136.0 s"]);
        assert_eq!(regressions(&base, &report(75_000, 8_000, false, 1_000_000_000), None, all), ["index ES: peak memory 2900 -> 8000 MB"]);
        assert!(regressions(&base, &report(136_000, 8_000, false, 1_000_000_000), None, sizes).is_empty(), "time and memory not compared");
        assert!(regressions(&base, &report(500_000, 9_000, true, 1_000_000_000), None, all).is_empty(), "a reused stage did not run");
        assert_eq!(regressions(&base, &report(75_000, 2_900, false, 1_200_000_000), None, sizes).len(), 1, "hot part a fifth larger");
        assert_eq!(regressions(&base, &base, Some(900_000_000), sizes).len(), 1, "above the objective");
    }
}
