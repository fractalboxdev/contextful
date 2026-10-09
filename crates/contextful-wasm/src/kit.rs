//! The connector authoring kit (`assurance.test.connector-kit`): a conformance suite an
//! author runs against a guest session, a recorded-HTTP replay server the guest reaches in
//! place of its vendor, and a property over position monotonicity.
//!
//! Every check answers `Err` with a sentence naming the table, position or field at fault,
//! so an author's test asserts `kit::conform(..).unwrap()` and reads the breach.

use crate::{batch, Cursor, Schema, Session};
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read as _, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// One row a batch carried, column name to value.
pub type Row = Map<String, Value>;

/// What one table's conformance read observed.
#[derive(Debug, Clone, PartialEq)]
pub struct TableReport {
    pub table: String,
    /// Batches the read yielded before exhaustion.
    pub batches: usize,
    pub rows: usize,
    /// The position after each batch, in order.
    pub positions: Vec<Cursor>,
}

/// A schema is valid when it is named, holds at least one field, names each field once,
/// and keys only on fields it holds.
pub fn validate(schema: &Schema) -> Result<(), String> {
    if schema.name.is_empty() {
        return Err("a discovered schema carries an empty name".into());
    }
    if schema.fields.is_empty() {
        return Err(format!("schema `{}` holds no field", schema.name));
    }
    let mut seen = BTreeSet::new();
    for f in &schema.fields {
        if f.name.is_empty() || !seen.insert(f.name.as_str()) {
            return Err(format!("schema `{}` names field `{}` twice or empty", schema.name, f.name));
        }
    }
    if let Some(k) = schema.primary_key.iter().find(|k| !seen.contains(k.as_str())) {
        return Err(format!("schema `{}` keys on `{k}`, which its primary key names but no field holds", schema.name));
    }
    Ok(())
}

/// Discovery returns valid schemas, each named once.
pub fn discover(session: &mut Session) -> Result<Vec<Schema>, String> {
    let schemas = session.discover().map_err(|f| format!("discovery failed: {f}"))?;
    let mut names = BTreeSet::new();
    for s in &schemas {
        validate(s)?;
        if !names.insert(s.name.clone()) {
            return Err(format!("discovery returns schema `{}` twice", s.name));
        }
    }
    Ok(schemas)
}

/// Open `table` from `from` and read to exhaustion within `max_batches`, returning the rows
/// and the position after each batch.
pub fn drain(session: &mut Session, table: &str, from: Option<&Cursor>, max_batches: usize) -> Result<(Vec<Row>, Vec<Cursor>), String> {
    read(session, table, from, max_batches).map(|(rows, positions, _)| (rows, positions))
}

/// A whole read: its rows, the position after each batch, and the rows each batch held.
type Read = (Vec<Row>, Vec<Cursor>, Vec<usize>);

/// [`drain`], plus the rows each batch held.
fn read(session: &mut Session, table: &str, from: Option<&Cursor>, max_batches: usize) -> Result<Read, String> {
    session.open(table, from).map_err(|f| format!("opening `{table}` failed: {f}"))?;
    let (mut rows, mut positions, mut sizes) = (Vec::new(), Vec::new(), Vec::new());
    loop {
        let next = session.next().map_err(|f| format!("reading `{table}` failed: {f}"))?;
        let Some(ipc) = next else { return Ok((rows, positions, sizes)) };
        if positions.len() == max_batches {
            return Err(format!("`{table}` did not exhaust within {max_batches} batch(es)"));
        }
        let got = batch::rows(&ipc).map_err(|f| format!("a `{table}` batch is no Arrow IPC: {f}"))?;
        sizes.push(got.len());
        rows.extend(got);
        positions.push(session.position().map_err(|f| format!("`{table}` reported no position: {f}"))?);
    }
}

/// The conformance suite over `tables`: each is a discovered schema, its read is finite and
/// lands only declared columns, and every position it reports reopens onto exactly the rows
/// after it.
pub fn conform(session: &mut Session, tables: &[&str], max_batches: usize) -> Result<Vec<TableReport>, String> {
    let schemas = discover(session)?;
    let mut reports = Vec::new();
    for &table in tables {
        let schema = schemas.iter().find(|s| s.name == table).ok_or_else(|| format!("table `{table}` is no discovered schema"))?;
        let (rows, positions, sizes) = read(session, table, None, max_batches)?;
        let declared: BTreeSet<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
        if let Some(col) = rows.iter().flat_map(|r| r.keys()).find(|k| !declared.contains(k.as_str())) {
            return Err(format!("`{table}` lands column `{col}`, which its schema does not declare"));
        }
        let mut before = 0;
        for (i, at) in positions.iter().enumerate() {
            before += sizes[i];
            let (rest, _) = drain(session, table, Some(at), max_batches)?;
            if rows[before..] != rest[..] {
                return Err(format!("`{table}` reopened at position {i} ({:?}) yields {} row(s), not the {} after it", at.bytes, rest.len(), rows.len() - before));
            }
        }
        reports.push(TableReport { table: table.to_string(), batches: positions.len(), rows: rows.len(), positions });
    }
    Ok(reports)
}

/// Positions never move back under `order`, the author's ordering of its own cursor bytes.
pub fn monotonic(positions: &[Cursor], order: impl Fn(&[u8], &[u8]) -> Ordering) -> Result<(), String> {
    for (i, w) in positions.windows(2).enumerate() {
        if order(&w[1].bytes, &w[0].bytes) == Ordering::Less {
            return Err(format!("position {} ({:?}) moved back from {:?}", i + 1, w[1].bytes, w[0].bytes));
        }
    }
    Ok(())
}

/// An order over unsigned decimals and big-endian integers: a longer value is later, and
/// values of one length compare byte by byte.
pub fn length_then_bytes(a: &[u8], b: &[u8]) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// The property: `cases` reads, each reopened from a position a full read reported and
/// chosen by a generator seeded with `seed`, report positions never before it and never
/// moving back. Returns the cases run.
pub fn monotonic_from_any_position(session: &mut Session, table: &str, max_batches: usize, order: impl Fn(&[u8], &[u8]) -> Ordering, cases: usize, seed: u64) -> Result<usize, String> {
    let (_, positions) = drain(session, table, None, max_batches)?;
    if positions.is_empty() {
        return Err(format!("`{table}` reported no position to reopen from"));
    }
    monotonic(&positions, &order)?;
    let mut state = seed | 1;
    for _ in 0..cases {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let start = &positions[(state % positions.len() as u64) as usize];
        let (_, after) = drain(session, table, Some(start), max_batches)?;
        let mut chain = vec![start.clone()];
        chain.extend(after);
        monotonic(&chain, &order).map_err(|e| format!("`{table}` reopened at {:?}: {e}", start.bytes))?;
    }
    Ok(cases)
}

/// One recorded request and the response it received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    pub method: String,
    pub path: String,
    pub status: u16,
    pub body: String,
}

/// A loopback server answering each request with the recorded exchange of its method and
/// path, and an unrecorded one with status 599, which it lists as a miss.
pub struct Replay {
    port: u16,
    served: Arc<Mutex<usize>>,
    misses: Arc<Mutex<Vec<String>>>,
}

impl Replay {
    pub fn start(recording: Vec<Exchange>) -> Replay {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("a bound address").port();
        let (served, misses): (Arc<Mutex<usize>>, Arc<Mutex<Vec<String>>>) = Default::default();
        let (s, m, rec) = (served.clone(), misses.clone(), Arc::new(recording));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (s, m, rec) = (s.clone(), m.clone(), rec.clone());
                std::thread::spawn(move || {
                    let _ = answer(stream, &rec, &s, &m);
                });
            }
        });
        Replay { port, served, misses }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// Requests answered from the recording.
    pub fn served(&self) -> usize {
        *self.served.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// `<method> <path>` of each request the recording holds no exchange for.
    pub fn misses(&self) -> Vec<String> {
        self.misses.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

fn answer(stream: TcpStream, recording: &[Exchange], served: &Mutex<usize>, misses: &Mutex<Vec<String>>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string());
    let mut len = 0;
    loop {
        let mut h = String::new();
        reader.read_line(&mut h)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    let path = target.split_once("://").map_or(target.as_str(), |(_, rest)| rest.find('/').map_or("/", |i| &rest[i..])).to_string();
    let (status, text) = match recording.iter().find(|e| e.method.eq_ignore_ascii_case(&method) && e.path == path) {
        Some(e) => {
            *served.lock().unwrap_or_else(|p| p.into_inner()) += 1;
            (e.status, e.body.clone())
        }
        None => {
            misses.lock().unwrap_or_else(|p| p.into_inner()).push(format!("{method} {path}"));
            (599, format!("no recorded exchange for {method} {path}"))
        }
    };
    let mut out = stream;
    write!(out, "HTTP/1.1 {status} R\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len())?;
    out.flush()
}
