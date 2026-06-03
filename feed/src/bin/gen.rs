//! `gen` — default-feature CLI that writes synthetic corpora (no async, no deps).
//!
//! Usage:
//!   gen --profile <steady|burst|flashcrash> --seed <u64> --events <n> --out <path>
//!   gen --all     # writes the canonical committed set under feed/corpus/
//!
//! `--all` writes the three reproducible Phase 4 fixtures at seed=1, 100k events:
//!   feed/corpus/{steady,burst,flashcrash}-s1-100k.mdf  + matching .meta.json
#![forbid(unsafe_code)]

use book::{Px, Qty};
use feed::synthetic::{GENERATOR_VERSION, GenConfig, Profile, generate};
use feed::Corpus;
use std::path::Path;
use std::process::ExitCode;

// Canonical committed-fixture parameters (the `--all` set).
const CANON_SEED: u64 = 1;
const CANON_EVENTS: usize = 100_000;
const CANON_MID: i64 = 65_000;
const CANON_BAND: i64 = 64;
const CANON_MAX_QTY: i64 = 1_024;
const CANON_START_TS: u64 = 0;

type CliError = Box<dyn std::error::Error>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = if args.iter().any(|a| a.as_str() == "--all") {
        write_all()
    } else {
        write_one(&args)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gen: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Write the three canonical synthetic corpora + provenance sidecars.
fn write_all() -> Result<(), CliError> {
    for profile in [Profile::Steady, Profile::Burst, Profile::FlashCrash] {
        let cfg = canon_cfg(profile);
        let name = profile_name(profile);
        let mdf = format!("feed/corpus/{name}-s1-100k.mdf");
        let meta = format!("feed/corpus/{name}-s1-100k.meta.json");
        write_corpus(&cfg, Path::new(&mdf), Path::new(&meta))?;
    }
    Ok(())
}

/// Parse the single-corpus CLI form and write it (+ a sibling `.meta.json`).
fn write_one(args: &[String]) -> Result<(), CliError> {
    let mut profile: Option<Profile> = None;
    let mut seed: Option<u64> = None;
    let mut events: Option<usize> = None;
    let mut out: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--profile" => profile = Some(parse_profile(next(args, &mut i)?)?),
            "--seed" => seed = Some(next(args, &mut i)?.parse()?),
            "--events" => events = Some(next(args, &mut i)?.parse()?),
            "--out" => out = Some(next(args, &mut i)?.to_owned()),
            other => return Err(format!("unknown argument: {other}").into()),
        }
        i += 1;
    }

    let profile = profile.ok_or("missing --profile")?;
    let out = out.ok_or("missing --out")?;
    let cfg = GenConfig {
        profile,
        seed: seed.ok_or("missing --seed")?,
        events: events.ok_or("missing --events")?,
        mid: Px(CANON_MID),
        band: CANON_BAND,
        max_qty: Qty(CANON_MAX_QTY),
        start_ts: CANON_START_TS,
    };
    write_corpus(&cfg, Path::new(&out), Path::new(&sibling_meta(&out)))?;
    Ok(())
}

fn write_corpus(cfg: &GenConfig, mdf: &Path, meta: &Path) -> Result<(), CliError> {
    let events = generate(cfg);
    Corpus::save(mdf, &events)?;
    std::fs::write(meta, meta_json(profile_name(cfg.profile), cfg))?;
    println!(
        "wrote {} ({} events) + {}",
        mdf.display(),
        events.len(),
        meta.display()
    );
    Ok(())
}

fn canon_cfg(profile: Profile) -> GenConfig {
    GenConfig {
        profile,
        seed: CANON_SEED,
        events: CANON_EVENTS,
        mid: Px(CANON_MID),
        band: CANON_BAND,
        max_qty: Qty(CANON_MAX_QTY),
        start_ts: CANON_START_TS,
    }
}

fn parse_profile(s: &str) -> Result<Profile, CliError> {
    match s {
        "steady" => Ok(Profile::Steady),
        "burst" => Ok(Profile::Burst),
        "flashcrash" => Ok(Profile::FlashCrash),
        other => Err(format!("unknown profile: {other} (steady|burst|flashcrash)").into()),
    }
}

fn profile_name(p: Profile) -> &'static str {
    match p {
        Profile::Steady => "steady",
        Profile::Burst => "burst",
        Profile::FlashCrash => "flashcrash",
    }
}

/// The value following `args[i]`, advancing `i` past it.
fn next<'a>(args: &'a [String], i: &mut usize) -> Result<&'a str, CliError> {
    *i += 1;
    args.get(*i)
        .map(String::as_str)
        .ok_or_else(|| "missing value for flag".into())
}

fn sibling_meta(out: &str) -> String {
    out.strip_suffix(".mdf")
        .map_or_else(|| format!("{out}.meta.json"), |stem| format!("{stem}.meta.json"))
}

/// Hand-rolled provenance JSON (§7) — no serde on the default tree.
fn meta_json(profile: &str, cfg: &GenConfig) -> String {
    format!(
        "{{\n  \"kind\": \"synthetic\",\n  \"profile\": \"{profile}\",\n  \"seed\": {seed},\n  \"events\": {events},\n  \"mid\": {mid},\n  \"band\": {band},\n  \"max_qty\": {max_qty},\n  \"start_ts\": {start_ts},\n  \"generator_version\": {ver}\n}}\n",
        seed = cfg.seed,
        events = cfg.events,
        mid = cfg.mid.ticks(),
        band = cfg.band,
        max_qty = cfg.max_qty.lots(),
        start_ts = cfg.start_ts,
        ver = GENERATOR_VERSION,
    )
}
