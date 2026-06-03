//! `recorder` — the QUARANTINED Binance recorder (feature `recorder`).
//!
//! Run **once**, by hand, to capture a real Binance session into a tick-space
//! corpus. It is async (`tokio`), feature-gated, and never on the measured path
//! or in CI. Everything it touches — websocket, REST, float-string parsing —
//! lives on THIS side of the corpus boundary; the moment a `BookEvent` is built
//! it is integer ticks, and nothing downstream ever sees a float or a `String`.
//!
//! Usage:
//!   recorder --symbol BTCUSDT --duration-secs 60 --out feed/corpus/btcusdt-sample.mdf
//!            [--max-events N] [--with-trades]
//!
//! Flow (Binance's documented depth-diff + local-book reconciliation, §6.2):
//!   1. GET /api/v3/exchangeInfo  -> tickSize (PRICE_FILTER), stepSize (LOT_SIZE).
//!   2. Open wss .../ws, SUBSCRIBE <sym>@depth@100ms (+ <sym>@trade), buffer diffs.
//!   3. GET /api/v3/depth?limit=1000 -> lastUpdateId + snapshot levels.
//!   4. Drop buffered diffs with u <= lastUpdateId; emit Clear + a Level per level.
//!   5. Apply first diff with U <= lastUpdateId+1 <= u; thereafter require U == prev_u+1.
//!      On a gap: log, re-snapshot (re-Clear + reseed).
//!   6. Optionally emit Trade (aggressor = if m { Ask } else { Bid }).
//!   7. seq = local counter; ts = exchange event time E(ms) * 1_000_000 (ns).
//!   8. On --duration-secs / --max-events / SIGINT: Corpus::save + .meta.json, exit 0.
//!
//! The string->tick conversion (§6.3) is EXACT integer arithmetic — no `f64`,
//! even here. `#![forbid(unsafe_code)]` holds.
#![forbid(unsafe_code)]
#![cfg(feature = "recorder")]
// Prose docs name exchange fields (tickSize, lastUpdateId, …) and the date math
// uses the conventional single-letter civil-calendar variables.
#![allow(clippy::doc_markdown, clippy::many_single_char_names)]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use book::{BookEvent, Px, Qty, Side};
use feed::Corpus;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const RECORDER_VERSION: u32 = 1;
const REST_HOST: &str = "api.binance.com";
const WS_URL: &str = "wss://stream.binance.com:9443/ws";

// ===================================================================
//  CLI
// ===================================================================

#[derive(Debug)]
struct Cli {
    symbol: String,
    duration_secs: u64,
    out: String,
    max_events: Option<u64>,
    with_trades: bool,
}

fn parse_cli() -> Result<Cli, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut symbol: Option<String> = None;
    let mut duration_secs: Option<u64> = None;
    let mut out: Option<String> = None;
    let mut max_events: Option<u64> = None;
    let mut with_trades = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--symbol" => symbol = Some(next(&args, &mut i)?.to_uppercase()),
            "--duration-secs" => {
                duration_secs = Some(next(&args, &mut i)?.parse().map_err(|e| format!("{e}"))?);
            }
            "--out" => out = Some(next(&args, &mut i)?.to_owned()),
            "--max-events" => {
                max_events = Some(next(&args, &mut i)?.parse().map_err(|e| format!("{e}"))?);
            }
            "--with-trades" => with_trades = true,
            other => return Err(format!("unknown argument: {other}")),
        }
        i += 1;
    }

    Ok(Cli {
        symbol: symbol.ok_or("missing --symbol")?,
        duration_secs: duration_secs.ok_or("missing --duration-secs")?,
        out: out.ok_or("missing --out")?,
        max_events,
        with_trades,
    })
}

fn next<'a>(args: &'a [String], i: &mut usize) -> Result<&'a str, String> {
    *i += 1;
    args.get(*i)
        .map(String::as_str)
        .ok_or_else(|| "missing value for flag".to_owned())
}

// ===================================================================
//  §6.3 — exact, f64-free string -> integer-tick conversion
// ===================================================================

#[derive(Debug)]
enum ConvError {
    NotDecimal,
    OffTickGrid,
    Overflow,
}

impl std::fmt::Display for ConvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvError::NotDecimal => write!(f, "not a decimal string"),
            ConvError::OffTickGrid => write!(f, "value is not an integer multiple of the tick"),
            ConvError::Overflow => write!(f, "tick count does not fit in i64"),
        }
    }
}

/// Number of fractional digits in a decimal string (chars after the `.`).
fn frac_digits(s: &str) -> usize {
    s.split_once('.').map_or(0, |(_, frac)| frac.len())
}

/// Parse a `[-]int[.frac]` decimal string into an `i128` scaled by `10^d`.
/// `d` must be >= the string's own fractional digit count. Exact; no `f64`.
fn scale_to_int(s: &str, d: usize) -> Result<i128, ConvError> {
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(ConvError::NotDecimal);
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
        || frac_part.len() > d
    {
        return Err(ConvError::NotDecimal);
    }

    let scale: i128 = 10_i128
        .checked_pow(u32::try_from(d).map_err(|_| ConvError::Overflow)?)
        .ok_or(ConvError::Overflow)?;
    let int_val: i128 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().map_err(|_| ConvError::Overflow)?
    };
    let frac_val: i128 = if frac_part.is_empty() {
        0
    } else {
        frac_part.parse().map_err(|_| ConvError::Overflow)?
    };
    // Pad the fractional part up to `d` digits: frac_val * 10^(d - frac_len).
    let pad: i128 = 10_i128
        .checked_pow(u32::try_from(d - frac_part.len()).map_err(|_| ConvError::Overflow)?)
        .ok_or(ConvError::Overflow)?;
    let magnitude = int_val
        .checked_mul(scale)
        .and_then(|x| x.checked_add(frac_val.checked_mul(pad)?))
        .ok_or(ConvError::Overflow)?;
    Ok(if neg { -magnitude } else { magnitude })
}

/// `value / tick` as an exact integer count, with no floating point.
/// Errors if `value` is not an integer multiple of `tick`.
fn to_ticks(value: &str, tick: &str) -> Result<i64, ConvError> {
    let d = frac_digits(value).max(frac_digits(tick));
    let v = scale_to_int(value, d)?;
    let t = scale_to_int(tick, d)?;
    if t == 0 || v % t != 0 {
        return Err(ConvError::OffTickGrid);
    }
    i64::try_from(v / t).map_err(|_| ConvError::Overflow)
}

// ===================================================================
//  Minimal HTTPS GET over the same rustls/ring stack wss uses.
//  (No extra HTTP-client crate; the recorder is off the measured path.)
// ===================================================================

fn tls_config() -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// HTTP/1.1 GET `path` from `REST_HOST` over TLS; returns the decoded body.
/// Uses `Connection: close` + read-to-EOF, de-chunking if the server chunks.
async fn https_get(tls: &Arc<ClientConfig>, path: &str) -> Result<Vec<u8>, String> {
    let connector = TlsConnector::from(tls.clone());
    let server = ServerName::try_from(REST_HOST.to_owned()).map_err(|e| format!("{e}"))?;
    let tcp = TcpStream::connect((REST_HOST, 443))
        .await
        .map_err(|e| format!("connect {REST_HOST}: {e}"))?;
    let mut stream = connector
        .connect(server, tcp)
        .await
        .map_err(|e| format!("tls handshake: {e}"))?;

    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {REST_HOST}\r\nUser-Agent: web3-terminal-recorder/1\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;
    stream.flush().await.map_err(|e| format!("flush: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .map_err(|e| format!("read: {e}"))?;
    parse_http_body(&raw)
}

/// Split an HTTP/1.1 response: require `200`, return the (de-chunked) body.
fn parse_http_body(raw: &[u8]) -> Result<Vec<u8>, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("malformed HTTP response (no header terminator)")?;
    let header = &raw[..split];
    let body = &raw[split + 4..];

    let header_str = String::from_utf8_lossy(header);
    let mut lines = header_str.split("\r\n");
    let status = lines.next().unwrap_or("");
    if !status.contains(" 200 ") {
        return Err(format!("HTTP status not 200: {status:?}"));
    }
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    if chunked {
        dechunk(body)
    } else {
        Ok(body.to_vec())
    }
}

/// Decode HTTP/1.1 chunked transfer-encoding.
fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(body.len());
    loop {
        let nl = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("chunked: missing size line")?;
        let size_str = std::str::from_utf8(&body[..nl]).map_err(|e| format!("chunk size: {e}"))?;
        let size_hex = size_str.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|e| format!("chunk size: {e}"))?;
        body = &body[nl + 2..];
        if size == 0 {
            break;
        }
        if body.len() < size + 2 {
            return Err("chunked: truncated chunk".to_owned());
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..]; // skip chunk data + trailing CRLF
    }
    Ok(out)
}

async fn get_json(tls: &Arc<ClientConfig>, path: &str) -> Result<Value, String> {
    let body = https_get(tls, path).await?;
    serde_json::from_slice(&body).map_err(|e| format!("json parse ({path}): {e}"))
}

// ===================================================================
//  Scale resolution + snapshot
// ===================================================================

struct Scales {
    tick_size: String,
    step_size: String,
}

/// §6.2.1 — PRICE_FILTER.tickSize and LOT_SIZE.stepSize from exchangeInfo.
async fn resolve_scales(tls: &Arc<ClientConfig>, symbol: &str) -> Result<Scales, String> {
    let path = format!("/api/v3/exchangeInfo?symbol={symbol}");
    let v = get_json(tls, &path).await?;
    let filters = v["symbols"]
        .get(0)
        .and_then(|s| s["filters"].as_array())
        .ok_or("exchangeInfo: no symbol/filters")?;
    let mut tick_size = None;
    let mut step_size = None;
    for f in filters {
        match f["filterType"].as_str() {
            Some("PRICE_FILTER") => tick_size = f["tickSize"].as_str().map(str::to_owned),
            Some("LOT_SIZE") => step_size = f["stepSize"].as_str().map(str::to_owned),
            _ => {}
        }
    }
    Ok(Scales {
        tick_size: tick_size.ok_or("exchangeInfo: no PRICE_FILTER.tickSize")?,
        step_size: step_size.ok_or("exchangeInfo: no LOT_SIZE.stepSize")?,
    })
}

struct Snapshot {
    last_update_id: u64,
    bids: Vec<(String, String)>,
    asks: Vec<(String, String)>,
}

/// §6.2.3 — GET /api/v3/depth?limit=1000.
async fn fetch_snapshot(tls: &Arc<ClientConfig>, symbol: &str) -> Result<Snapshot, String> {
    let path = format!("/api/v3/depth?symbol={symbol}&limit=1000");
    let v = get_json(tls, &path).await?;
    let last_update_id = v["lastUpdateId"].as_u64().ok_or("depth: no lastUpdateId")?;
    Ok(Snapshot {
        last_update_id,
        bids: levels_of(&v["bids"])?,
        asks: levels_of(&v["asks"])?,
    })
}

/// Parse a `[["px","qty"], ...]` array into owned decimal-string pairs.
fn levels_of(v: &Value) -> Result<Vec<(String, String)>, String> {
    v.as_array()
        .ok_or("depth: levels not an array")?
        .iter()
        .map(|e| {
            let px = e[0].as_str().ok_or("depth: px not a string")?;
            let qty = e[1].as_str().ok_or("depth: qty not a string")?;
            Ok((px.to_owned(), qty.to_owned()))
        })
        .collect()
}

// ===================================================================
//  Emission helpers (the corpus boundary lives here)
// ===================================================================

struct Emitter<'a> {
    events: &'a mut Vec<BookEvent>,
    seq: u64,
    scales: &'a Scales,
}

impl Emitter<'_> {
    /// Reset to empty, then seed from a REST snapshot (ts = 0; pre-stream).
    fn seed_snapshot(&mut self, snap: &Snapshot) -> Result<(), String> {
        self.events.push(BookEvent::clear(self.seq, 0));
        self.seq += 1;
        for (side, levels) in [(Side::Bid, &snap.bids), (Side::Ask, &snap.asks)] {
            for (px, qty) in levels {
                self.level(0, side, px, qty)?;
            }
        }
        Ok(())
    }

    fn level(&mut self, ts_ms: u64, side: Side, px: &str, qty: &str) -> Result<(), String> {
        let px = to_ticks(px, &self.scales.tick_size).map_err(|e| format!("px {px:?}: {e}"))?;
        let qty = to_ticks(qty, &self.scales.step_size).map_err(|e| format!("qty {qty:?}: {e}"))?;
        self.events
            .push(BookEvent::level(self.seq, ts_ms * 1_000_000, side, Px(px), Qty(qty)));
        self.seq += 1;
        Ok(())
    }

    fn trade(&mut self, ts_ms: u64, aggressor: Side, px: &str, qty: &str) -> Result<(), String> {
        let px =
            to_ticks(px, &self.scales.tick_size).map_err(|e| format!("trade px {px:?}: {e}"))?;
        let qty =
            to_ticks(qty, &self.scales.step_size).map_err(|e| format!("trade qty {qty:?}: {e}"))?;
        self.events
            .push(BookEvent::trade(self.seq, ts_ms * 1_000_000, aggressor, Px(px), Qty(qty)));
        self.seq += 1;
        Ok(())
    }
}

/// Apply one depthUpdate's bid/ask entries as absolute Level updates.
fn apply_diff(em: &mut Emitter<'_>, e_ms: u64, diff: &Value) -> Result<(), String> {
    for (side, key) in [(Side::Bid, "b"), (Side::Ask, "a")] {
        if let Some(arr) = diff[key].as_array() {
            for entry in arr {
                let px = entry[0].as_str().ok_or("diff: px not a string")?;
                let qty = entry[1].as_str().ok_or("diff: qty not a string")?;
                em.level(e_ms, side, px, qty)?;
            }
        }
    }
    Ok(())
}

// ===================================================================
//  main
// ===================================================================

#[tokio::main(flavor = "multi_thread")]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(n) => {
            println!("recorder: wrote {n} events");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("recorder: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn run() -> Result<usize, String> {
    let cli = parse_cli()?;
    // rustls 0.23 picks its single enabled provider (ring) for `builder()`.
    let tls = tls_config();

    let scales = resolve_scales(&tls, &cli.symbol).await?;
    eprintln!(
        "recorder: {} tickSize={} stepSize={}",
        cli.symbol, scales.tick_size, scales.step_size
    );

    // 2. Open ws and SUBSCRIBE before snapshotting, so diffs are buffered.
    let (mut ws, _resp) = connect_async(WS_URL).await.map_err(|e| format!("ws connect: {e}"))?;
    let sym_lower = cli.symbol.to_lowercase();
    let mut params = vec![format!("{sym_lower}@depth@100ms")];
    if cli.with_trades {
        params.push(format!("{sym_lower}@trade"));
    }
    let sub = serde_json::json!({ "method": "SUBSCRIBE", "params": params, "id": 1 }).to_string();
    ws.send(Message::Text(sub.into()))
        .await
        .map_err(|e| format!("ws subscribe: {e}"))?;

    // Reader task owns the stream: forwards text frames, answers pings. The
    // unbounded channel itself buffers diffs that arrive before/while we snapshot.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Text(t)) if tx.send(t.as_str().to_owned()).is_err() => break,
                Ok(Message::Ping(p)) => {
                    let _ = ws.send(Message::Pong(p)).await;
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    // 3-4. Snapshot, seed local book.
    let mut snap = fetch_snapshot(&tls, &cli.symbol).await?;
    let mut events: Vec<BookEvent> = Vec::new();
    let mut em = Emitter { events: &mut events, seq: 0, scales: &scales };
    em.seed_snapshot(&snap)?;
    let mut last_update_id = snap.last_update_id;
    let mut prev_u: u64 = 0;
    let mut synced = false;

    // 8. Bounded by duration / max-events / SIGINT.
    let deadline = tokio::time::sleep(Duration::from_secs(cli.duration_secs));
    tokio::pin!(deadline);
    let mut resnapshots: u32 = 0;

    loop {
        tokio::select! {
            biased;
            () = &mut deadline => break,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("recorder: SIGINT — flushing");
                break;
            }
            msg = rx.recv() => {
                let Some(text) = msg else { break };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                match v["e"].as_str() {
                    Some("depthUpdate") => {
                        let (Some(uu), Some(uu_first), Some(e_ms)) =
                            (v["u"].as_u64(), v["U"].as_u64(), v["E"].as_u64())
                        else { continue };
                        if synced {
                            if uu_first == prev_u + 1 {
                                apply_diff(&mut em, e_ms, &v)?;
                                prev_u = uu;
                            } else {
                                eprintln!(
                                    "recorder: gap (U={uu_first} != prev_u+1={}) — re-snapshot",
                                    prev_u + 1
                                );
                                snap = resnapshot(&tls, &cli.symbol, &mut em, &mut resnapshots).await?;
                                last_update_id = snap.last_update_id;
                                synced = false;
                            }
                        } else if uu <= last_update_id {
                            continue; // stale buffered diff
                        } else if uu_first <= last_update_id + 1 && last_update_id < uu {
                            apply_diff(&mut em, e_ms, &v)?;
                            prev_u = uu;
                            synced = true;
                        } else {
                            // Missed the seam between snapshot and first usable diff.
                            snap = resnapshot(&tls, &cli.symbol, &mut em, &mut resnapshots).await?;
                            last_update_id = snap.last_update_id;
                            synced = false;
                        }
                    }
                    Some("trade") if cli.with_trades => {
                        let (Some(e_ms), Some(px), Some(qty)) =
                            (v["E"].as_u64(), v["p"].as_str(), v["q"].as_str())
                        else { continue };
                        let aggressor = if v["m"].as_bool().unwrap_or(false) {
                            Side::Ask
                        } else {
                            Side::Bid
                        };
                        em.trade(e_ms, aggressor, px, qty)?;
                    }
                    _ => {} // subscribe ack / other
                }

                if let Some(max) = cli.max_events {
                    if em.seq >= max {
                        eprintln!("recorder: reached --max-events {max}");
                        break;
                    }
                }
            }
        }
    }

    let count = events.len();
    Corpus::save(std::path::Path::new(&cli.out), &events).map_err(|e| format!("save: {e}"))?;
    write_meta(&cli, &scales, count)?;
    eprintln!(
        "recorder: saved {count} events to {} ({resnapshots} re-snapshots)",
        cli.out
    );
    Ok(count)
}

/// Re-fetch the snapshot and re-seed (Clear + levels) on a sequence gap.
async fn resnapshot(
    tls: &Arc<ClientConfig>,
    symbol: &str,
    em: &mut Emitter<'_>,
    counter: &mut u32,
) -> Result<Snapshot, String> {
    *counter += 1;
    let snap = fetch_snapshot(tls, symbol).await?;
    em.seed_snapshot(&snap)?;
    Ok(snap)
}

// ===================================================================
//  Provenance sidecar (§7, recorded)
// ===================================================================

fn write_meta(cli: &Cli, scales: &Scales, record_count: usize) -> Result<(), String> {
    let sym_lower = cli.symbol.to_lowercase();
    let ws_sub = if cli.with_trades {
        format!("{WS_URL} (SUBSCRIBE {sym_lower}@depth@100ms/{sym_lower}@trade)")
    } else {
        format!("{WS_URL} (SUBSCRIBE {sym_lower}@depth@100ms)")
    };
    let meta = serde_json::json!({
        "kind": "binance-recorded",
        "symbol": cli.symbol,
        "tick_size": scales.tick_size,
        "step_size": scales.step_size,
        "captured_at_utc": iso8601_utc_now(),
        "duration_secs": cli.duration_secs,
        "record_count": record_count,
        "with_trades": cli.with_trades,
        "recorder_version": RECORDER_VERSION,
        "binance_endpoints": [
            format!("https://{REST_HOST}/api/v3/exchangeInfo?symbol={}", cli.symbol),
            format!("https://{REST_HOST}/api/v3/depth?symbol={}&limit=1000", cli.symbol),
            ws_sub,
        ],
    });
    let path = sibling_meta(&cli.out);
    let text = serde_json::to_string_pretty(&meta).map_err(|e| format!("meta json: {e}"))?;
    std::fs::write(&path, format!("{text}\n")).map_err(|e| format!("write meta: {e}"))?;
    Ok(())
}

fn sibling_meta(out: &str) -> String {
    out.strip_suffix(".mdf")
        .map_or_else(|| format!("{out}.meta.json"), |stem| format!("{stem}.meta.json"))
}

/// ISO-8601 UTC timestamp with no extra crates (civil-from-days, Hinnant).
fn iso8601_utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // days since 1970-01-01 -> civil (y, m, d), Howard Hinnant's algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}
