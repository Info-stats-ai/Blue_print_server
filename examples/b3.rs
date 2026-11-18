//! Prints the BLAKE3 hash of stdin, for the end-to-end scripts.
use std::io::Read as _;

fn main() {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).expect("read stdin");
    println!("{}", blake3::hash(&input).to_hex());
}
