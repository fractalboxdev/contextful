//! The peak heap one page decode holds over the page's bytes: an 8 MiB JSON Lines body, an
//! 8 MiB JSON body read at `/data`, and an 8 MiB 61-column tab-separated body; the figure is
//! the largest of the three ratios.

#[path = "../measure.rs"]
mod measure;

use contextful_decode::{decode, decode_delimited, Delimited, Format};
use contextful_eval::record;
use measure::Draw;

const ID: &str = "backfill-decode-peak-heap";
const SEED: u64 = 0x5eed_0b03;
const COLUMNS: usize = 61;

#[global_allocator]
static ALLOCATOR: measure::PeakAllocator = measure::PeakAllocator;

fn object(i: usize, draw: &mut Draw) -> String {
    format!(
        "{{\"id\":{i},\"symbol\":\"SYM{:04}\",\"date\":\"2030-01-{:02}\",\"open\":{:.4},\"close\":{:.4},\"volume\":{},\"venue\":\"xnas\"}}",
        draw.next() % 5000,
        1 + i % 28,
        draw.unit() + 100.0,
        draw.unit() + 100.0,
        draw.next() % 1_000_000
    )
}

fn jsonl(bytes: usize, draw: &mut Draw) -> Vec<u8> {
    let mut body = String::new();
    for i in 0.. {
        if body.len() >= bytes {
            break;
        }
        body.push_str(&object(i, draw));
        body.push('\n');
    }
    body.into_bytes()
}

fn json(bytes: usize, draw: &mut Draw) -> Vec<u8> {
    let mut body = String::from("{\"data\":[");
    for i in 0.. {
        if body.len() >= bytes {
            break;
        }
        if i > 0 {
            body.push(',');
        }
        body.push_str(&object(i, draw));
    }
    body.push_str("]}");
    body.into_bytes()
}

/// GDELT-shaped event lines: identifiers, dates, actor codes, counts and coordinates.
fn tsv(bytes: usize, draw: &mut Draw) -> Vec<u8> {
    let mut body = String::new();
    for i in 0.. {
        if body.len() >= bytes {
            break;
        }
        let cells: Vec<String> = (0..COLUMNS)
            .map(|c| match c % 4 {
                0 => format!("{}", 1_000_000_000 + i * COLUMNS + c),
                1 => format!("USA{:03}", draw.next() % 1000),
                2 => String::new(),
                _ => format!("{:.4}", draw.unit() * 90.0),
            })
            .collect();
        body.push_str(&cells.join("\t"));
        body.push('\n');
    }
    body.into_bytes()
}

fn ratio(body: &[u8], f: impl FnOnce(&[u8]) -> usize) -> f64 {
    let (peak, rows) = measure::peak_heap_of(|| f(body));
    assert!(rows > 0, "the body decoded no row");
    peak as f64 / body.len() as f64
}

fn main() {
    let bytes = if measure::small() { 1 << 20 } else { 8 << 20 };
    let mut draw = Draw::new(SEED);
    let columns: Vec<String> = (0..COLUMNS).map(|c| format!("c{c}")).collect();
    let tab = Delimited { delimiter: '\t', columns: Some(columns) };
    let ratios = [
        ratio(&jsonl(bytes, &mut draw), |b| decode(Format::Jsonl, b, None, "page.jsonl").unwrap().0.len()),
        ratio(&json(bytes, &mut draw), |b| decode(Format::Json, b, Some("/data"), "page.json").unwrap().0.len()),
        ratio(&tsv(bytes, &mut draw), |b| decode_delimited(Format::Csv, b, None, "page.tsv", None, &tab).unwrap().0.len()),
    ];
    let worst = ratios.into_iter().fold(0.0, f64::max);
    record::emit(ID, worst, 3, SEED);
}
