//! Sending only what changed in a file instead of the whole file.
//!
//! A patch is a list of line operations over the version it was made from:
//! keep the next `k` lines, drop the next `d` lines, or insert text `i`. It
//! names that version by its SHA-256 (`base_hash`) and the result by its own
//! (`hash`), so whoever applies it can tell for certain whether their copy is
//! the right starting point; if not, they ask for the whole file instead. A
//! patch never produces a different file than the full send would.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PatchOp {
    Keep { k: usize },
    Delete { d: usize },
    Insert { i: String },
}

/// The SHA-256 of `content`, as lowercase hex.
pub fn content_hash(content: &str) -> String {
    Sha256::digest(content.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The line operations that turn `old` into `new`. Clients make patches;
/// the server only applies them, so this is here for its tests.
#[cfg(test)]
pub fn diff(old: &str, new: &str) -> Vec<PatchOp> {
    use similar::{ChangeTag, TextDiff};
    let mut ops: Vec<PatchOp> = Vec::new();
    for change in TextDiff::from_lines(old, new).iter_all_changes() {
        match (change.tag(), ops.last_mut()) {
            (ChangeTag::Equal, Some(PatchOp::Keep { k })) => *k += 1,
            (ChangeTag::Equal, _) => ops.push(PatchOp::Keep { k: 1 }),
            (ChangeTag::Delete, Some(PatchOp::Delete { d })) => *d += 1,
            (ChangeTag::Delete, _) => ops.push(PatchOp::Delete { d: 1 }),
            (ChangeTag::Insert, Some(PatchOp::Insert { i })) => i.push_str(change.value()),
            (ChangeTag::Insert, _) => ops.push(PatchOp::Insert { i: change.value().to_string() }),
        }
    }
    ops
}

/// `base` with `ops` applied, or `None` when they don't fit it (too few
/// lines, or lines left over).
pub fn apply(base: &str, ops: &[PatchOp]) -> Option<String> {
    let lines: Vec<&str> = base.split_inclusive('\n').collect();
    let mut at = 0usize;
    let mut out = String::with_capacity(base.len());
    for op in ops {
        match op {
            PatchOp::Keep { k } => {
                let end = at.checked_add(*k).filter(|end| *end <= lines.len())?;
                lines[at..end].iter().for_each(|line| out.push_str(line));
                at = end;
            }
            PatchOp::Delete { d } => {
                at = at.checked_add(*d).filter(|end| *end <= lines.len())?;
            }
            PatchOp::Insert { i } => out.push_str(i),
        }
    }
    (at == lines.len()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patch_rebuilds_the_new_version_exactly() {
        let cases = [
            ("", "a\nb\n"),
            ("a\nb\n", ""),
            ("a\nb\nc\n", "a\nB\nc\nd\n"),
            ("no newline at end", "no newline at end\nnow there is\n"),
            ("x\ny", "x\nz"),
            ("same\n", "same\n"),
            ("héllo\nwörld\n", "héllo\nthere\nwörld\n"),
        ];
        for (old, new) in cases {
            let ops = diff(old, new);
            assert_eq!(apply(old, &ops).as_deref(), Some(new), "{old:?} -> {new:?}");
        }
    }

    #[test]
    fn a_patch_for_another_version_is_refused() {
        let ops = diff("a\nb\nc\n", "a\nc\n");
        assert_eq!(apply("a\n", &ops), None);
        assert_eq!(apply("a\nb\nc\nd\n", &ops), None);
    }

    #[test]
    fn a_small_change_to_a_big_file_is_a_small_patch() {
        let old: String = (0..2000).map(|n| format!("line {n}\n")).collect();
        let new = old.replace("line 1000\n", "line one thousand\n");
        let patch = serde_json::to_string(&diff(&old, &new)).unwrap();
        assert!(patch.len() < 100, "{} bytes: {patch}", patch.len());
        assert!(old.len() > 15_000);
    }

    #[test]
    fn ops_have_a_compact_wire_form() {
        let ops = vec![PatchOp::Keep { k: 2 }, PatchOp::Delete { d: 1 }, PatchOp::Insert { i: "x\n".into() }];
        let wire = serde_json::to_string(&ops).unwrap();
        assert_eq!(wire, r#"[{"k":2},{"d":1},{"i":"x\n"}]"#);
        assert_eq!(serde_json::from_str::<Vec<PatchOp>>(&wire).unwrap(), ops);
    }
}
