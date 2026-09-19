//! The node record (§3) and its SQLite mapping.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Done,
    Failed,
    Killed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Killed => "killed",
        }
    }
    /// `None` for anything but the four statuses (format 1's `pruned` included).
    pub fn parse(s: &str) -> Option<Status> {
        match s {
            "running" => Some(Status::Running),
            "done" => Some(Status::Done),
            "failed" => Some(Status::Failed),
            "killed" => Some(Status::Killed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub parent: Option<String>,
    /// git tree hash of the code manifest
    pub code: String,
    pub config: String,
    pub data: String,
    pub env: String,
    pub recipe_hash: String,
    pub weights: Option<String>,
    pub docs: Option<String>,
    pub fork_step: Option<i64>,
    pub status: Status,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub command: String,
    pub note: Option<String>,
    /// note was generated from the config delta
    pub note_auto: bool,
    pub lock_ok: Option<bool>,
    pub depth: i64,
    /// sweep name when this node is a fan-out member
    pub sweep: Option<String>,
    /// when the node was pruned (hidden, weights collectable); `None` = live.
    /// Last and skipped when `None`, so an unpruned node's remote line is byte-identical
    /// to format 1's and sync markers written by 0.1 stay valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pruned_at: Option<String>,
}

pub const COLS: &str = "id,parent,code,config,data,env,recipe_hash,weights,docs,fork_step,status,created_at,finished_at,command,note,note_auto,lock_ok,depth,sweep,pruned_at";

pub fn recipe_hash(code: &str, config: &str, data: &str, env: &str) -> String {
    let mut h = blake3::Hasher::new();
    for p in [code, config, data, env] {
        h.update(p.as_bytes());
    }
    h.finalize().to_hex().to_string()
}

impl Node {
    pub fn from_row(r: &Row) -> rusqlite::Result<Node> {
        let id: String = r.get(0)?;
        let status: String = r.get(10)?;
        let status = Status::parse(&status).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                10,
                rusqlite::types::Type::Text,
                format!("node {id} has unknown status '{status}'").into(),
            )
        })?;
        Ok(Node {
            id,
            parent: r.get(1)?,
            code: r.get(2)?,
            config: r.get(3)?,
            data: r.get(4)?,
            env: r.get(5)?,
            recipe_hash: r.get(6)?,
            weights: r.get(7)?,
            docs: r.get(8)?,
            fork_step: r.get(9)?,
            status,
            created_at: r.get(11)?,
            finished_at: r.get(12)?,
            command: r.get(13)?,
            note: r.get(14)?,
            note_auto: r.get(15)?,
            lock_ok: r.get(16)?,
            depth: r.get(17)?,
            sweep: r.get(18)?,
            pruned_at: r.get(19)?,
        })
    }

    pub fn insert(&self, db: &Connection) -> Result<()> {
        db.execute(
            &format!("INSERT INTO nodes ({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)"),
            params![
                self.id,
                self.parent,
                self.code,
                self.config,
                self.data,
                self.env,
                self.recipe_hash,
                self.weights,
                self.docs,
                self.fork_step,
                self.status.as_str(),
                self.created_at,
                self.finished_at,
                self.command,
                self.note,
                self.note_auto,
                self.lock_ok,
                self.depth,
                self.sweep,
                self.pruned_at
            ],
        )?;
        Ok(())
    }

    /// First line of the note, for `tree`.
    pub fn title(&self) -> &str {
        self.note
            .as_deref()
            .and_then(|n| n.lines().next())
            .unwrap_or("")
    }
}

pub fn get(db: &Connection, id: &str) -> Result<Option<Node>> {
    Ok(db
        .query_row(
            &format!("SELECT {COLS} FROM nodes WHERE id=?1"),
            [id],
            Node::from_row,
        )
        .optional()?)
}

pub fn all(db: &Connection) -> Result<Vec<Node>> {
    let mut st = db.prepare(&format!("SELECT {COLS} FROM nodes ORDER BY created_at, id"))?;
    let v = st
        .query_map([], Node::from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

pub fn children(db: &Connection, id: &str) -> Result<Vec<Node>> {
    let mut st = db.prepare(&format!(
        "SELECT {COLS} FROM nodes WHERE parent=?1 ORDER BY created_at, id"
    ))?;
    let v = st
        .query_map([id], Node::from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(v)
}

/// Ancestor chain from `id` up to the root, starting with `id` itself.
pub fn ancestry(db: &Connection, id: &str) -> Result<Vec<Node>> {
    let mut out = vec![];
    let mut cur = Some(id.to_string());
    while let Some(c) = cur {
        let Some(n) = get(db, &c)? else { break };
        cur = n.parent.clone();
        out.push(n);
    }
    Ok(out)
}
