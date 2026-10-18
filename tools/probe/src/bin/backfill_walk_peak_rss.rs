//! The peak resident set of a process whose watermarked HTTP walk pulls 999 pages of 100
//! rows from a loopback vendor, the median of five fresh children. Each child prints its
//! peak and the most rows one pull returned.

#[path = "../measure.rs"]
mod measure;

use contextful_connectors::http::{HttpConfig, HttpSource};
use contextful_core::ports::FixedClock;
use contextful_core::run::ports::{Cancellation, PullRequest, Source};
use contextful_core::time::Instant;
use contextful_eval::record;
use contextful_outbound::Resolver;
use measure::Draw;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

const ID: &str = "backfill-walk-peak-rss";
const SEED: u64 = 0x5eed_0b04;
const ROWS_PER_PAGE: usize = 100;
const CHILDREN: usize = 5;

struct Never;

impl Cancellation for Never {
    fn requested(&self) -> bool {
        false
    }
}

/// Page `p` of `pages` holds 100 seeded rows; any later page is empty.
fn page(p: usize, pages: usize) -> String {
    if p == 0 || p > pages {
        return "[]".into();
    }
    let mut draw = Draw::new(SEED ^ p as u64);
    let rows: Vec<Value> = (0..ROWS_PER_PAGE)
        .map(|i| json!({"id": format!("w{p}-{i}"), "at": "2030-01-02", "symbol": format!("SYM{:04}", draw.next() % 5000), "close": draw.unit() + 100.0}))
        .collect();
    serde_json::to_string(&rows).unwrap()
}

fn answer(stream: TcpStream, pages: usize) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        if header.trim_end().is_empty() {
            break;
        }
    }
    let target = line.split_whitespace().nth(1).unwrap_or_default();
    let p = target
        .split_once('?')
        .map(|(_, q)| q.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == "p").and_then(|(_, v)| v.parse().ok()).unwrap_or(1))
        .unwrap_or(1);
    let body = page(p, pages);
    let mut out = stream;
    write!(out, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())?;
    out.flush()
}

fn vendor(pages: usize) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let _ = answer(stream, pages);
            });
        }
    });
    port
}

/// One watermarked walk over the vendor at `port`, pulled until a pull reports no more and
/// each pull's rows dropped before the next, as the runner stages them: the rows it
/// returned and the most one pull returned.
fn walk(port: u16) -> (usize, usize) {
    let config = HttpConfig::parse(&json!({"endpoint": format!("http://127.0.0.1:{port}/v1"), "page_param": "p", "since_param": "since"})).unwrap();
    let clock = Arc::new(FixedClock(Instant::parse("2030-01-01T00:00:00Z").unwrap()));
    let resolver = Arc::new(Resolver::new(vec![], false, clock));
    let mut source = HttpSource::new(config, "t", resolver).unwrap().watermarked();
    let mut position = Some(json!({"field": "at", "at": "2030-01-01"}));
    let (mut rows, mut most) = (0, 0);
    for ordinal in 0.. {
        let request = PullRequest { step_label: format!("pull-{ordinal}"), position, idempotency_key: format!("idem-{ordinal}") };
        let pulled: Value = serde_json::from_slice(&source.pull(&request, &Never).unwrap()).unwrap();
        let n = pulled["rows"].as_array().map_or(0, Vec::len);
        (rows, most) = (rows + n, most.max(n));
        if pulled["more"] != json!(true) {
            break;
        }
        position = pulled.get("cursor").cloned();
    }
    (rows, most)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--walk") {
        let (port, pages): (u16, usize) = (args[2].parse().unwrap(), args[3].parse().unwrap());
        let (rows, most) = walk(port);
        assert_eq!(rows, pages * ROWS_PER_PAGE, "the walk returned {rows} rows");
        println!("{} {most}", measure::peak_rss_mib());
        return;
    }
    let pages = if measure::small() { 50 } else { 999 };
    let port = vendor(pages).to_string();
    let peaks = (0..CHILDREN).map(|_| measure::child(&["--walk", &port, &pages.to_string()]).split_whitespace().next().unwrap().parse().unwrap()).collect();
    record::emit(ID, measure::median(peaks), (pages * ROWS_PER_PAGE) as u64, SEED);
}
