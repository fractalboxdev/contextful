//! Reads one differential case on standard input and prints the native build's decision, as
//! `contextful formal differential --decide` does, from the same `contextful_policy::decide`
//! module. It links the decision module alone, so it starts in milliseconds where the
//! engine binary takes a quarter second; the differential suite's stand-in references start
//! it once per case.

use std::io::Read;

fn main() -> std::io::Result<()> {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let decision = contextful_policy::decide::decide(&input);
    println!("{}", serde_json::to_string(&decision).map_err(std::io::Error::other)?);
    Ok(())
}
