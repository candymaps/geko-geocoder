//! The debugging commands of the geocoder, run as `geko geocoder <command> …` on the segments
//! that the last build left in the data directory.

use std::fmt::Write as _;
use std::path::Path;

use clap::{CommandFactory, Parser, Subcommand};
use geko_core::country::CountryCode;
use geko_core::{GekoError, Result};
use geko_module_sdk::module::CommandInfo;

use crate::pipeline::segment_dir;
use crate::sources;

#[derive(Parser)]
#[command(name = "geko geocoder", about = "Debugging aids of the geocoder, on the segments of the last build")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show how a query is interpreted.
    Parse {
        text: String,
        #[arg(long, default_value = "ES")]
        country: CountryCode,
    },
    /// Search the built segment, with the time of the first and of a warm query.
    Query {
        text: String,
        #[arg(long, default_value = "ES")]
        country: CountryCode,
        /// Treat the last word as a prefix being typed.
        #[arg(long)]
        autocomplete: bool,
        /// Bias results towards "lat,lon".
        #[arg(long)]
        focus: Option<String>,
        #[arg(long, default_value_t = 10)]
        size: usize,
        /// Print the time of each interpretation of the query (stderr).
        #[arg(long)]
        debug: bool,
    },
    /// Reverse geocode a point.
    Reverse {
        lat: f64,
        lon: f64,
        #[arg(long, default_value = "ES")]
        country: CountryCode,
        #[arg(long, default_value_t = 5)]
        size: usize,
    },
}

/// The commands, for the host to list.
pub fn list() -> Vec<CommandInfo> {
    Cli::command()
        .get_subcommands()
        .map(|c| CommandInfo { name: c.get_name().to_owned(), about: c.get_about().map(|a| a.to_string()).unwrap_or_default() })
        .collect()
}

pub fn run(data_dir: &Path, name: &str, args: &[String]) -> Result<String> {
    let argv = ["geko geocoder", name].into_iter().map(str::to_owned).chain(args.iter().cloned());
    let cli = match Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        // `--help` is no failure: its text is what the command prints.
        Err(e) if matches!(e.kind(), clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion) => return Ok(e.to_string()),
        Err(e) => return Err(GekoError::InvalidInput(e.to_string())),
    };
    let mut out = String::new();
    match cli.command {
        Command::Parse { text, country } => {
            let parsed = sources::parser(country)?.parse(&text)?;
            for i in parsed.interpretations {
                writeln!(out, "{:>5.1}%  {:?}", i.probability * 100.0, i.kind).unwrap();
            }
        }
        Command::Query { text, country, autocomplete, focus, size, debug } => {
            let dir = segment_dir(data_dir, country).join("geocode");
            let opened = std::time::Instant::now();
            let engine = sources::geocode_engine(&dir, sources::normalizer(country)?, false)?;
            let open_ms = opened.elapsed().as_secs_f64() * 1000.0;
            let parser = sources::parser(country)?;
            let mut ctx = geko_core::query::QueryContext { size, ..Default::default() };
            if let Some(f) = focus {
                let invalid = || GekoError::InvalidInput("--focus must be lat,lon".into());
                let (lat, lon) = f.split_once(',').ok_or_else(invalid)?;
                let (lat, lon) = (lat.trim().parse().map_err(|_| invalid())?, lon.trim().parse().map_err(|_| invalid())?);
                ctx.focus = Some(geko_core::geo::Coord::from_degrees(lat, lon)?);
            }
            let mut hits = Vec::new();
            let started = std::time::Instant::now();
            let parsed = parser.parse(&text)?;
            if autocomplete {
                engine.autocomplete(&parsed, &ctx, &mut hits)?;
            } else {
                engine.search(&parsed, &ctx, &mut hits)?;
            }
            let cold_us = started.elapsed().as_micros();
            let runs = 200;
            let warm = std::time::Instant::now();
            for _ in 0..runs {
                let parsed = parser.parse(&text)?;
                if autocomplete { engine.autocomplete(&parsed, &ctx, &mut hits)? } else { engine.search(&parsed, &ctx, &mut hits)? }
            }
            let warm_us = warm.elapsed().as_micros() / runs;
            if debug {
                // Once more, warm, timing each interpretation.
                let debug_ctx = geko_core::query::QueryContext { debug: true, ..ctx.clone() };
                let parsed = parser.parse(&text)?;
                if autocomplete { engine.autocomplete(&parsed, &debug_ctx, &mut hits)? } else { engine.search(&parsed, &debug_ctx, &mut hits)? }
            }
            for h in &hits {
                let Some(place) = engine.place(h.id) else { continue };
                let props: serde_json::Value =
                    serde_json::from_slice(&[b"{".as_slice(), &place.json, b"}"].concat()).unwrap_or_default();
                writeln!(
                    out,
                    "{:>7.3}  {:<13} {:<12} {}  [{}]",
                    h.score,
                    place.kind.as_str(),
                    h.match_type.as_str(),
                    props["label"].as_str().unwrap_or(&place.name),
                    place.public_id
                )
                .unwrap();
            }
            writeln!(out, "open {open_ms:.1} ms · first query {cold_us} µs · warm {warm_us} µs/query").unwrap();
        }
        Command::Reverse { lat, lon, country, size } => {
            let segment = segment_dir(data_dir, country);
            let opened = std::time::Instant::now();
            let reverse = sources::reverse_index(&segment.join("reverse"))?;
            let engine = sources::geocode_engine(&segment.join("geocode"), sources::normalizer(country)?, false)?;
            let open_ms = opened.elapsed().as_millis();
            let point = geko_core::geo::Coord::from_degrees(lat, lon)?;
            let mut hits = Vec::new();
            let started = std::time::Instant::now();
            reverse.nearest(point, geko_core::query::KindFilter::ALL, size, &mut hits)?;
            let chain = reverse.admin_at(point);
            let query_us = started.elapsed().as_micros();
            for h in &hits {
                let Some(place) = engine.place(h.id) else { continue };
                let props: serde_json::Value = serde_json::from_slice(&[b"{".as_slice(), &place.json, b"}"].concat()).unwrap_or_default();
                let metres = geko_geom::distance_m((point.lon_e7(), point.lat_e7()), (place.point.lon_e7(), place.point.lat_e7()));
                writeln!(out, "{metres:>7.1} m  {:<13} {}  [{}]", place.kind.as_str(), props["label"].as_str().unwrap_or(&place.name), place.public_id)
                    .unwrap();
            }
            if let Some(chain) = chain {
                let levels: Vec<String> = chain.levels.iter().map(|l| format!("{} {} ({})", l.kind, l.name, l.code)).collect();
                writeln!(out, "admin: {}", levels.join(" > ")).unwrap();
            }
            writeln!(out, "open {open_ms} ms · query {query_us} µs").unwrap();
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_its_commands_and_answers_help() {
        let names: Vec<String> = list().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["parse", "query", "reverse"]);
        let help = run(Path::new("/nonexistent"), "query", &["--help".into()]).unwrap();
        assert!(help.contains("--autocomplete"), "{help}");
        assert!(run(Path::new("/nonexistent"), "unknown", &[]).is_err());
    }
}
