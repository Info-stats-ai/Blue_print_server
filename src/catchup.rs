//! Catching a client up on a session's files with as little sent as possible.
//!
//! 1. The client sends one fingerprint of every file it already knows
//!    (`root`). If it matches the server's, the reply is "up to date": one
//!    small message each way, however big the project.
//! 2. Otherwise the server sends the list of files with their SHA-256s. The
//!    client compares that with its own copies and asks only for the files
//!    that differ, listing the chunks it already has of each.
//! 3. The server answers each with a recipe: "your chunk n" or new text. The
//!    client rebuilds the file and checks it against the SHA-256; a file that
//!    doesn't match is asked for again whole.
//!
//! Chunks are cut by `store::chunk_text`, the same way the app cuts them.
//! Large messages travel zstd-compressed in binary frames.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::patch::content_hash;

/// Messages longer than this go compressed to clients that take it.
pub const COMPRESS_OVER: usize = 16 * 1024;
/// The most a compressed frame may expand to.
pub const MAX_EXPANDED: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ManifestEntry {
    pub path: String,
    pub hash: String,
    pub size: usize,
}

/// The session's files, as path, SHA-256 and size, sorted by path. Files the
/// server never shares are left out.
pub fn manifest(files: &HashMap<String, String>) -> Vec<ManifestEntry> {
    let mut entries: Vec<ManifestEntry> = files
        .iter()
        .filter(|(path, content)| crate::guard::file_refusal(path, content).is_none())
        .map(|(path, content)| ManifestEntry { path: path.clone(), hash: content_hash(content), size: content.len() })
        .collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
}

/// One fingerprint for a whole set of (path, SHA-256) pairs. The app computes
/// the same over the versions it knows.
pub fn root<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut pairs: Vec<(&str, &str)> = entries.into_iter().collect();
    pairs.sort();
    let mut hasher = blake3::Hasher::new();
    for (path, hash) in pairs {
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(hash.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

/// The name a chunk goes by between the server and the app: the first 16
/// bytes of its BLAKE3 hash, in hex.
pub fn chunk_name(chunk: &str) -> String {
    blake3::hash(chunk.as_bytes()).to_hex()[..32].to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Part {
    /// The client's own chunk at this index.
    Have { c: usize },
    /// New text.
    Text { t: String },
}

/// How to build `content` from the chunks the client has (`have`, in its
/// order): reuse what it has, send the rest as text.
pub fn recipe(content: &str, have: &[String]) -> Vec<Part> {
    let mut index: HashMap<&str, usize> = HashMap::with_capacity(have.len());
    for (position, name) in have.iter().enumerate() {
        index.entry(name.as_str()).or_insert(position);
    }
    let mut parts: Vec<Part> = Vec::new();
    for chunk in crate::store::chunk_text(content) {
        match index.get(chunk_name(chunk).as_str()) {
            Some(&position) => parts.push(Part::Have { c: position }),
            None => match parts.last_mut() {
                Some(Part::Text { t }) => t.push_str(chunk),
                _ => parts.push(Part::Text { t: chunk.to_string() }),
            },
        }
    }
    parts
}

/// `text` as a frame for a client: compressed when it's long and the client
/// takes compressed frames.
pub fn frame(text: String, compressed: bool) -> axum::extract::ws::Message {
    use axum::extract::ws::Message;
    if compressed && text.len() > COMPRESS_OVER
        && let Ok(bytes) = zstd::bulk::compress(text.as_bytes(), 3)
    {
        return Message::Binary(bytes.into());
    }
    Message::Text(text.into())
}

/// The text of a compressed frame from a client, refusing anything that
/// expands past `MAX_EXPANDED`.
pub fn unframe(bytes: &[u8]) -> Option<String> {
    use std::io::Read as _;
    let mut decoder = zstd::stream::read::Decoder::new(bytes).ok()?.take(MAX_EXPANDED as u64 + 1);
    let mut text = Vec::new();
    decoder.read_to_end(&mut text).ok()?;
    if text.len() > MAX_EXPANDED {
        return None;
    }
    String::from_utf8(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rebuild(parts: &[Part], have: &[&str]) -> String {
        parts
            .iter()
            .map(|part| match part {
                Part::Have { c } => have[*c].to_string(),
                Part::Text { t } => t.clone(),
            })
            .collect()
    }

    fn code(lines: usize, tag: &str) -> String {
        (0..lines).map(|n| format!("fn {tag}_{n}() -> u32 {{ {n} * 7 + {} }}\n", n % 13)).collect()
    }

    #[test]
    fn a_recipe_rebuilds_the_file_exactly_and_sends_only_what_changed() {
        let old = code(3000, "f");
        let new = old.replacen("fn f_1500()", "fn f_1500_renamed()", 1);
        let have: Vec<&str> = crate::store::chunk_text(&old);
        let names: Vec<String> = have.iter().map(|chunk| chunk_name(chunk)).collect();
        let parts = recipe(&new, &names);
        assert_eq!(rebuild(&parts, &have), new);
        let sent: usize = parts.iter().map(|part| if let Part::Text { t } = part { t.len() } else { 0 }).sum();
        assert!(sent * 20 < new.len(), "sent {sent} bytes of {}", new.len());
        // With nothing to start from, it's the whole file as text.
        let whole = recipe(&new, &[]);
        assert_eq!(whole, vec![Part::Text { t: new.clone() }]);
    }

    #[test]
    fn the_root_ignores_order_and_changes_with_any_file() {
        let a = root([("a.py", "1"), ("b.py", "2")]);
        assert_eq!(a, root([("b.py", "2"), ("a.py", "1")]));
        assert_ne!(a, root([("a.py", "1"), ("b.py", "3")]));
        assert_ne!(a, root([("a.py", "1")]));
        // Paths and hashes can't run into each other.
        assert_ne!(root([("ab", "c")]), root([("a", "bc")]));
    }

    #[test]
    fn the_manifest_leaves_secrets_out_and_is_sorted() {
        let files: HashMap<String, String> =
            [("b.py".into(), "x".into()), ("a.py".into(), "y".into()), (".env".into(), "K=1".into())].into();
        let entries = manifest(&files);
        assert_eq!(entries.iter().map(|entry| entry.path.as_str()).collect::<Vec<_>>(), ["a.py", "b.py"]);
        assert_eq!(entries[0].hash, content_hash("y"));
    }

    #[test]
    fn frames_compress_large_text_and_refuse_bombs() {
        let small = "hello".to_string();
        assert!(matches!(frame(small, true), axum::extract::ws::Message::Text(_)));
        let large = code(5000, "g");
        let axum::extract::ws::Message::Binary(bytes) = frame(large.clone(), true) else { panic!("not compressed") };
        assert!(bytes.len() * 4 < large.len());
        assert_eq!(unframe(&bytes).as_deref(), Some(large.as_str()));
        assert!(matches!(frame(large, false), axum::extract::ws::Message::Text(_)), "only for clients that take it");
        let bomb = zstd::bulk::compress(&vec![b'a'; MAX_EXPANDED + 10], 3).unwrap();
        assert_eq!(unframe(&bomb), None);
        assert_eq!(unframe(b"not zstd"), None);
    }
}
