//! The probe guest. Each table the host opens exercises one property of the host:
//!
//! | table            | `next` does                                                        |
//! | ---------------- | ------------------------------------------------------------------ |
//! | `items`          | three rows over every scalar type, two per batch, resumable by id |
//! | `spin`           | loops without end                                                  |
//! | `grow`           | allocates without end                                              |
//! | `log`            | logs 2048 messages of 1 KiB, then ends                             |
//! | `ambient`        | one row naming what the environment, arguments and root hold      |
//! | `calls`          | one row naming the calls the session received, in order          |
//! | `fetch <url>`    | one GET; a 429 is a rate-limited error, a transport failure transient |
//! | `swallow <url>`  | one GET whose failure the guest ignores, then ends                 |
//! | `burst <n> <url>`| `n` GETs issued before any is awaited; one row counting the 200s   |
//! | `fail`           | a transient error                                                  |

#[cfg(not(feature = "optional"))]
wit_bindgen::generate!({
    path: "../../../wit",
    world: "contextful:connector/source-connector@1.2.0",
    generate_all,
});

#[cfg(feature = "optional")]
wit_bindgen::generate!({
    path: "../../../wit",
    inline: "
        package probe:guest;
        world probe {
            include contextful:connector/configurable-source-connector@1.2.0;
            include contextful:connector/attributing-source-connector@1.2.0;
        }
    ",
    world: "probe:guest/probe",
    generate_all,
});

use arrow_array::{
    ArrayRef, BooleanArray, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray, BinaryArray,
    TimestampMillisecondArray,
};
use arrow_ipc::writer::StreamWriter;
use contextful::connector::types::{Cursor, CursorKind, DataType, Error, Schema, SchemaField};
use exports::contextful::connector::source::{Guest, GuestReadHandle, ReadHandle};
use std::cell::RefCell;
use std::sync::Arc;
use wasi::http::outgoing_handler;
use wasi::http::types::{Fields, IncomingBody, OutgoingBody, OutgoingRequest, Scheme};
use wasi::io::streams::StreamError;
use wasi::logging::logging::{log, Level};

thread_local! {
    static CALLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static CONFIG: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn called(name: &str) {
    CALLS.with(|c| c.borrow_mut().push(name.to_string()));
}

fn ipc(batch: &RecordBatch) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut w = StreamWriter::try_new(&mut out, &batch.schema()).expect("ipc writer");
        w.write(batch).expect("ipc write");
        w.finish().expect("ipc finish");
    }
    out
}

fn text_batch(column: &str, value: &str) -> Vec<u8> {
    let schema = arrow_schema::Schema::new(vec![arrow_schema::Field::new(column, arrow_schema::DataType::Utf8, false)]);
    let batch = RecordBatch::try_new(Arc::new(schema), vec![Arc::new(StringArray::from(vec![value])) as ArrayRef]).expect("batch");
    ipc(&batch)
}

fn items(ids: &[i64]) -> Vec<u8> {
    use arrow_schema::{DataType as A, Field, TimeUnit};
    let schema = arrow_schema::Schema::new(vec![
        Field::new("id", A::Int64, false),
        Field::new("flag", A::Boolean, false),
        Field::new("small", A::Int32, false),
        Field::new("ratio", A::Float64, false),
        Field::new("name", A::Utf8, false),
        Field::new("blob", A::Binary, false),
        Field::new("at", A::Timestamp(TimeUnit::Millisecond, None), false),
        Field::new("doc", A::Utf8, true),
    ]);
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(ids.to_vec())),
        Arc::new(BooleanArray::from(ids.iter().map(|i| i % 2 == 0).collect::<Vec<_>>())),
        Arc::new(Int32Array::from(ids.iter().map(|i| *i as i32 * 10).collect::<Vec<_>>())),
        Arc::new(Float64Array::from(ids.iter().map(|i| *i as f64 / 2.0).collect::<Vec<_>>())),
        Arc::new(StringArray::from(ids.iter().map(|i| format!("item-{i}")).collect::<Vec<_>>())),
        Arc::new(BinaryArray::from_iter_values(ids.iter().map(|i| vec![*i as u8]))),
        Arc::new(TimestampMillisecondArray::from(ids.iter().map(|i| 1_700_000_000_000 + i).collect::<Vec<_>>())),
        Arc::new(StringArray::from(ids.iter().map(|i| Some(format!("{{\"n\":{i}}}"))).collect::<Vec<_>>())),
    ];
    ipc(&RecordBatch::try_new(Arc::new(schema), cols).expect("batch"))
}

fn request(url: &str) -> Result<outgoing_handler::FutureIncomingResponse, String> {
    let (scheme, rest) = url.split_once("://").ok_or("no scheme")?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let req = OutgoingRequest::new(Fields::new());
    req.set_scheme(Some(&if scheme == "https" { Scheme::Https } else { Scheme::Http })).map_err(|_| "scheme")?;
    req.set_authority(Some(authority)).map_err(|_| "authority")?;
    req.set_path_with_query(Some(&path)).map_err(|_| "path")?;
    let body = req.body().map_err(|_| "body")?;
    OutgoingBody::finish(body, None).map_err(|e| format!("{e:?}"))?;
    outgoing_handler::handle(req, None).map_err(|e| format!("{e:?}"))
}

/// Status, headers and body of one answered request.
fn answer(fut: outgoing_handler::FutureIncomingResponse) -> Result<(u16, Vec<(String, Vec<u8>)>, Vec<u8>), String> {
    fut.subscribe().block();
    let resp = fut.get().ok_or("no response")?.map_err(|_| "response taken")?.map_err(|e| format!("{e:?}"))?;
    let status = resp.status();
    let headers = resp.headers().entries();
    let body = resp.consume().map_err(|_| "consume")?;
    let mut buf = Vec::new();
    {
        let stream = body.stream().map_err(|_| "stream")?;
        loop {
            match stream.blocking_read(65536) {
                Ok(chunk) => buf.extend(chunk),
                Err(StreamError::Closed) => break,
                Err(e) => return Err(format!("{e:?}")),
            }
        }
    }
    let _ = IncomingBody::finish(body);
    Ok((status, headers, buf))
}

fn get(url: &str) -> Result<(u16, Vec<(String, Vec<u8>)>, Vec<u8>), String> {
    answer(request(url)?)
}

struct Probe;

enum Mode {
    Items { ids: Vec<i64>, at: usize, last: i64 },
    Spin,
    Grow,
    Log,
    Once(Vec<u8>),
    Done,
    Fetch(String),
    Swallow(String),
    Burst(usize, String),
}

pub struct Handle {
    mode: RefCell<Mode>,
}

impl Guest for Probe {
    type ReadHandle = Handle;

    fn cursor_kind() -> CursorKind {
        CursorKind::Monotonic
    }

    fn discover() -> Result<Vec<Schema>, Error> {
        called("discover");
        let f = |name: &str, ty: DataType, nullable: bool| SchemaField { name: name.into(), ty, nullable };
        Ok(vec![Schema {
            name: "items".into(),
            fields: vec![
                f("id", DataType::Int64, false),
                f("flag", DataType::Boolean, false),
                f("small", DataType::Int32, false),
                f("ratio", DataType::Float64, false),
                f("name", DataType::String, false),
                f("blob", DataType::Bytes, false),
                f("at", DataType::TimestampMillis, false),
                f("doc", DataType::Json, true),
            ],
            primary_key: vec!["id".into()],
        }])
    }

    fn open(table: String, from: Option<Cursor>) -> Result<ReadHandle, Error> {
        called("open");
        let mut words = table.split_whitespace();
        let mode = match words.next().unwrap_or_default() {
            "items" => {
                let after = from.map(|c| String::from_utf8_lossy(&c.bytes).parse::<i64>().unwrap_or(0)).unwrap_or(0);
                Mode::Items { ids: (1..=3).filter(|i| *i > after).collect(), at: 0, last: after }
            }
            "spin" => Mode::Spin,
            "grow" => Mode::Grow,
            "log" => Mode::Log,
            "ambient" => {
                let env = std::env::vars().count();
                let args = std::env::args().count();
                let root = std::fs::read_dir("/").map(|d| d.count().to_string()).unwrap_or_else(|_| "none".into());
                Mode::Once(text_batch("ambient", &format!("env={env} args={args} root={root}")))
            }
            "calls" => Mode::Once(text_batch("calls", &CALLS.with(|c| c.borrow().join(",")))),
            "config" => Mode::Once(text_batch("config", &CONFIG.with(|c| c.borrow().clone().unwrap_or_default()))),
            "fetch" => Mode::Fetch(words.next().unwrap_or_default().to_string()),
            "swallow" => Mode::Swallow(words.next().unwrap_or_default().to_string()),
            "burst" => {
                let n = words.next().and_then(|n| n.parse().ok()).unwrap_or(1);
                Mode::Burst(n, words.next().unwrap_or_default().to_string())
            }
            "fail" => return Err(Error::Transient("simulated".into())),
            other => return Err(Error::Permanent(format!("no table `{other}`"))),
        };
        Ok(ReadHandle::new(Handle { mode: RefCell::new(mode) }))
    }
}

impl GuestReadHandle for Handle {
    fn next(&self) -> Result<Option<Vec<u8>>, Error> {
        let mut mode = self.mode.borrow_mut();
        match &mut *mode {
            Mode::Items { ids, at, last } => {
                if *at >= ids.len() {
                    return Ok(None);
                }
                let end = (*at + 2).min(ids.len());
                let chunk = ids[*at..end].to_vec();
                *at = end;
                *last = *chunk.last().unwrap_or(last);
                Ok(Some(items(&chunk)))
            }
            Mode::Spin => {
                let mut n: u64 = 0;
                loop {
                    n = std::hint::black_box(n.wrapping_add(1));
                }
            }
            Mode::Grow => {
                let mut hog: Vec<Vec<u8>> = Vec::new();
                loop {
                    hog.push(vec![1u8; 4 * 1024 * 1024]);
                    std::hint::black_box(&hog);
                }
            }
            Mode::Log => {
                let line = "x".repeat(1024);
                for _ in 0..2048 {
                    log(Level::Info, "probe", &line);
                }
                *mode = Mode::Done;
                Ok(None)
            }
            Mode::Once(bytes) => {
                let out = std::mem::take(bytes);
                *mode = Mode::Done;
                Ok(Some(out))
            }
            Mode::Done => Ok(None),
            Mode::Fetch(url) => {
                let url = url.clone();
                *mode = Mode::Done;
                let (status, headers, body) = get(&url).map_err(Error::Transient)?;
                if status == 429 {
                    let secs = headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("retry-after"))
                        .and_then(|(_, v)| String::from_utf8_lossy(v).trim().parse().ok())
                        .unwrap_or(0);
                    return Err(Error::RateLimited(secs));
                }
                Ok(Some(text_batch("body", &format!("{status} {}", String::from_utf8_lossy(&body)))))
            }
            Mode::Swallow(url) => {
                let url = url.clone();
                *mode = Mode::Done;
                let _ = get(&url);
                Ok(None)
            }
            Mode::Burst(n, url) => {
                let (n, url) = (*n, url.clone());
                *mode = Mode::Done;
                let pending: Vec<_> = (0..n).map(|_| request(&url)).collect::<Result<_, _>>().map_err(Error::Transient)?;
                let ok = pending.into_iter().map(answer).filter(|r| matches!(r, Ok((200, _, _)))).count();
                Ok(Some(text_batch("ok", &ok.to_string())))
            }
        }
    }

    fn position(&self) -> Cursor {
        let last = match &*self.mode.borrow() {
            Mode::Items { last, .. } => *last,
            _ => 0,
        };
        Cursor { kind: CursorKind::Monotonic, bytes: last.to_string().into_bytes() }
    }
}

#[cfg(feature = "optional")]
impl exports::contextful::connector::config::Guest for Probe {
    fn configure(config: String) -> Result<(), Error> {
        called("configure");
        if config.contains("\"unknown\"") {
            return Err(Error::Permanent("the probe reads no key `unknown`".into()));
        }
        CONFIG.with(|c| *c.borrow_mut() = Some(config));
        Ok(())
    }
}

#[cfg(feature = "optional")]
impl exports::contextful::connector::attribution::Guest for Probe {
    /// 600 short values after one 600-byte value, when the configuration asks for them.
    fn failed_partitions() -> Vec<String> {
        let wants = CONFIG.with(|c| c.borrow().as_deref().is_some_and(|s| s.contains("\"flood\"")));
        if !wants {
            return vec!["tenant-a".into(), "Tenant-A ".into()];
        }
        let mut out = vec!["x".repeat(600)];
        out.extend((0..600).map(|i| format!("p{i}")));
        out
    }
}

export!(Probe);
