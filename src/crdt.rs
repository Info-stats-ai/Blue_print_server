//! A JSON-serializable mirror of `text::Operation` for the websocket wire
//! protocol. `text::Operation` itself doesn't derive `Serialize`/`Deserialize`
//! (nor does `clock::Global`, which it embeds) — real Zed collab moves these
//! over protobuf via a separate `proto` crate, which is far heavier than this
//! server wants to depend on. This module is the minimal hand-written bridge
//! for the plain-JSON protocol blueprint-server already uses everywhere else.

use std::ops::Range;

use serde::{Deserialize, Serialize};
use text::{EditOperation, Operation, UndoOperation};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireLamport {
    pub value: u32,
    pub replica_id: u16,
}

impl From<clock::Lamport> for WireLamport {
    fn from(timestamp: clock::Lamport) -> Self {
        Self {
            value: timestamp.value,
            replica_id: timestamp.replica_id.as_u16(),
        }
    }
}

impl From<WireLamport> for clock::Lamport {
    fn from(wire: WireLamport) -> Self {
        clock::Lamport {
            value: wire.value,
            replica_id: clock::ReplicaId::new(wire.replica_id),
        }
    }
}

/// `clock::Global` is just a per-replica sequence-number vector internally;
/// this mirrors it as a plain array indexed by replica id.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WireGlobal(pub Vec<u32>);

impl From<&clock::Global> for WireGlobal {
    fn from(global: &clock::Global) -> Self {
        WireGlobal(global.iter().map(|timestamp| timestamp.value).collect())
    }
}

impl From<WireGlobal> for clock::Global {
    fn from(wire: WireGlobal) -> Self {
        let mut global = clock::Global::new();
        for (replica_id, value) in wire.0.into_iter().enumerate() {
            if value > 0 {
                global.observe(clock::Lamport {
                    value,
                    replica_id: clock::ReplicaId::new(replica_id as u16),
                });
            }
        }
        global
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireEditOperation {
    pub timestamp: WireLamport,
    pub version: WireGlobal,
    pub ranges: Vec<(usize, usize)>,
    pub new_text: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireUndoOperation {
    pub timestamp: WireLamport,
    pub version: WireGlobal,
    pub counts: Vec<(WireLamport, u32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum WireOperation {
    Edit(WireEditOperation),
    Undo(WireUndoOperation),
}

impl From<&Operation> for WireOperation {
    fn from(op: &Operation) -> Self {
        match op {
            Operation::Edit(edit) => WireOperation::Edit(WireEditOperation {
                timestamp: edit.timestamp.into(),
                version: (&edit.version).into(),
                ranges: edit
                    .ranges
                    .iter()
                    .map(|range| (range.start.0, range.end.0))
                    .collect(),
                new_text: edit.new_text.iter().map(|text| text.to_string()).collect(),
            }),
            Operation::Undo(undo) => WireOperation::Undo(WireUndoOperation {
                timestamp: undo.timestamp.into(),
                version: (&undo.version).into(),
                counts: undo
                    .counts
                    .iter()
                    .map(|(timestamp, count)| ((*timestamp).into(), *count))
                    .collect(),
            }),
        }
    }
}

impl From<WireOperation> for Operation {
    fn from(wire: WireOperation) -> Self {
        match wire {
            WireOperation::Edit(edit) => Operation::Edit(EditOperation {
                timestamp: edit.timestamp.into(),
                version: edit.version.into(),
                ranges: edit
                    .ranges
                    .into_iter()
                    .map(|(start, end)| Range {
                        start: text::FullOffset(start),
                        end: text::FullOffset(end),
                    })
                    .collect(),
                new_text: edit.new_text.into_iter().map(|text| text.into()).collect(),
            }),
            WireOperation::Undo(undo) => Operation::Undo(UndoOperation {
                timestamp: undo.timestamp.into(),
                version: undo.version.into(),
                counts: undo
                    .counts
                    .into_iter()
                    .map(|(timestamp, count)| (timestamp.into(), count))
                    .collect(),
            }),
        }
    }
}
