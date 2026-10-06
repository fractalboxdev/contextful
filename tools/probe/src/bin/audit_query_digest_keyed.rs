//! A low-entropy statement dictionary cannot recover a keyed audit query digest.

use contextful_eval::record;
use contextful_policy::audit::query_digest;
use sha2::{Digest, Sha256};

const ID: &str = "audit-query-digest-keyed";
const SEED: u64 = 0x5eed_0081;

fn main() {
    let key = [0x81; 32];
    let statements: Vec<String> = (0..256)
        .map(|i| format!("select * from table_{i}"))
        .collect();
    let target = query_digest(&key, &statements[42]);
    let digest = target
        .rsplit_once(':')
        .map(|(_, hex)| hex)
        .expect("a tagged digest");
    let matches = statements
        .iter()
        .filter(|statement| hex::encode(Sha256::digest(statement.as_bytes())) == digest)
        .count();
    record::emit(ID, matches as f64, statements.len() as u64, SEED);
    if matches != 0 {
        std::process::exit(1);
    }
}
