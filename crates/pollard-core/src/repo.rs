//! Repository handle: `.pollard/` layout, SQLite (WAL), file lock, node resolution.

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};

use crate::config::{self, Config};
use crate::{Error, IoCtx, Result, msg, node};

pub struct Repo {
    pub root: PathBuf,
    pub dot: PathBuf,
    pub db: Connection,
    pub config: Config,
    pub objects: pollard_objects::Store,
    /// Command line recorded in the op log (set by the CLI).
    pub cmdline: String,
}

/// Format-1 (0.1.0-alpha.1) schema, frozen: new repos start here and run `MIGRATIONS`.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS nodes(
  id TEXT PRIMARY KEY, parent TEXT, code TEXT NOT NULL, config TEXT NOT NULL, data TEXT NOT NULL,
  env TEXT NOT NULL, recipe_hash TEXT NOT NULL, weights TEXT, docs TEXT, fork_step INTEGER,
  status TEXT NOT NULL, created_at TEXT NOT NULL, finished_at TEXT, command TEXT NOT NULL,
  note TEXT, note_auto INTEGER NOT NULL DEFAULT 0, lock_ok INTEGER, depth INTEGER NOT NULL, sweep TEXT);
CREATE INDEX IF NOT EXISTS nodes_recipe ON nodes(recipe_hash);
CREATE INDEX IF NOT EXISTS nodes_parent ON nodes(parent);
CREATE TABLE IF NOT EXISTS pins(name TEXT PRIMARY KEY, node_id TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS ops(op_id INTEGER PRIMARY KEY, ts TEXT NOT NULL, command TEXT NOT NULL,
  before_snapshot BLOB NOT NULL, after_snapshot BLOB NOT NULL, wc_snapshot TEXT);
CREATE TABLE IF NOT EXISTS deltas(node_id TEXT PRIMARY KEY, config_delta TEXT, code_delta TEXT,
  data_delta TEXT, env_delta TEXT);
CREATE TABLE IF NOT EXISTS metrics(node_id TEXT NOT NULL, key TEXT NOT NULL, step INTEGER NOT NULL,
  value REAL NOT NULL, ts TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS metrics_idx ON metrics(node_id, key, step);
CREATE TABLE IF NOT EXISTS code_trees(git_tree TEXT PRIMARY KEY, manifest_hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS data_cache(path TEXT PRIMARY KEY, mtime INTEGER NOT NULL, size INTEGER NOT NULL, hash TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#;

/// Repo format this build reads and writes (`PRAGMA user_version`; 0.1's databases read 0).
pub const FORMAT: i64 = 2;
/// The database file. Format 1 used `db.sqlite`, which is now a stub directory, so 0.1
/// binaries fail to open a migrated repo instead of misreading it.
const DB: &str = "state.sqlite";
const LEGACY_DB: &str = "db.sqlite";

/// `MIGRATIONS[i]` takes a database to format `i + 2`, inside the caller's transaction,
/// and returns extra lines for the upgrade notice.
type Migration = fn(&Connection) -> Result<Vec<String>>;
const MIGRATIONS: &[Migration] = &[to_format2];

const UPGRADED_TXT: &str = "This repo was upgraded to format 2 by pollard 0.2.0-alpha.1. The database is now .pollard/state.sqlite.
Install pollard 0.2.0-alpha.1 or newer to use it.
The old database is in .pollard/backup/. To roll back, see the upgrade notes in pollard's CHANGELOG.md.
";

fn newer(format: i64) -> Error {
    msg(format!(
        "this repo uses format {format}, written by a newer pollard; this pollard ({}) reads up to format {FORMAT}. Upgrade pollard.",
        env!("CARGO_PKG_VERSION")
    ))
}

fn user_version(db: &Connection) -> Result<i64> {
    Ok(db.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

/// `user_version` from a database file's header, without opening (or writing) it.
fn header_version(path: &Path) -> Result<i64> {
    use std::io::Read;
    let mut h = [0u8; 64];
    File::open(path).at(path)?.read_exact(&mut h).at(path)?;
    Ok(i32::from_be_bytes([h[60], h[61], h[62], h[63]]).into())
}

/// Open `.pollard/state.sqlite`, first creating it (new repo) or migrating a format-1
/// `db.sqlite` into it. Refuses a format newer than `FORMAT` before writing anything.
fn open_db(dot: &Path) -> Result<Connection> {
    let state = dot.join(DB);
    let legacy = dot.join(LEGACY_DB);
    // Refuse a newer format before touching anything (not even the lock file).
    for f in [&state, &legacy] {
        if f.is_file() {
            let v = header_version(f)?;
            if v > FORMAT {
                return Err(newer(v));
            }
            break;
        }
    }
    if !state.exists() || !legacy.is_dir() {
        let _lock = lock_at(dot)?;
        upgrade(dot)?;
    }
    let db = Connection::open(&state)?;
    db.busy_timeout(std::time::Duration::from_secs(30))?;
    let v = user_version(&db)?;
    if v > FORMAT {
        return Err(newer(v));
    }
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "NORMAL")?;
    if v < FORMAT {
        let _lock = lock_at(dot)?;
        migrate(&db)?;
    }
    Ok(db)
}

/// Test hook (debug builds): exit at a named migration step to simulate a crash.
fn abort_at(step: &str) {
    if cfg!(debug_assertions)
        && std::env::var("POLLARD_TEST_MIGRATION_ABORT").is_ok_and(|s| s == step)
    {
        eprintln!("pollard: test abort at migration step {step}");
        std::process::exit(99);
    }
}

/// Create or finish `state.sqlite` (caller holds `.pollard/lock`). Every step can be
/// interrupted and redone: the migration runs on `state.sqlite.tmp`, the rename is the
/// commit point, then the old file becomes the backup and `db.sqlite/` the stub.
fn upgrade(dot: &Path) -> Result<()> {
    let state = dot.join(DB);
    let legacy = dot.join(LEGACY_DB);
    let tmp = dot.join("state.sqlite.tmp");
    if tmp.exists() {
        fs::remove_file(&tmp).at(&tmp)?;
    }
    if !state.exists() {
        if legacy.is_file() {
            let old = Connection::open(&legacy)?;
            old.busy_timeout(std::time::Duration::from_secs(30))?;
            old.execute(
                "VACUUM INTO ?1",
                [tmp.to_str()
                    .ok_or_else(|| msg(format!("{}: path is not UTF-8", tmp.display())))?],
            )?;
            drop(old);
            abort_at("tmp_written");
            let db = Connection::open(&tmp)?;
            let lines = migrate(&db)?;
            let mut notice = format!(
                "pollard: upgraded this repo to format {FORMAT} (pollard {})\n  backup of the old database: {{backup}}\n",
                env!("CARGO_PKG_VERSION")
            );
            for l in lines {
                notice.push_str(&format!("  {l}\n"));
            }
            notice.push_str("  po undo cannot go back past this upgrade, and pollard 0.1.0-alpha.1 can no longer open this repo\n  to roll back, see the upgrade notes: https://github.com/thinhngo-x/pollard/blob/main/CHANGELOG.md#upgrade-notes\n");
            db.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES('upgrade_notice',?1)",
                [notice],
            )?;
            drop(db);
            abort_at("tmp_migrated");
            fs::rename(&tmp, &state).at(&state)?;
            abort_at("renamed");
        } else {
            // new repo: the frozen format-1 schema, then every migration
            let db = Connection::open(&state)?;
            db.execute_batch(SCHEMA)?;
            migrate(&db)?;
        }
    }
    let db = Connection::open(&state)?;
    db.busy_timeout(std::time::Duration::from_secs(30))?;
    if legacy.is_file() {
        // Standalone backup: fold the WAL into the file, then move the file itself.
        let old = Connection::open(&legacy)?;
        old.busy_timeout(std::time::Duration::from_secs(30))?;
        old.pragma_update(None, "journal_mode", "DELETE")?;
        drop(old);
        let dir = dot.join("backup");
        fs::create_dir_all(&dir).at(&dir)?;
        let mut backup = dir.join("db-format1.sqlite");
        if backup.exists() {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            backup = dir.join(format!("db-format1-{secs}.sqlite"));
        }
        let shown = format!(
            ".pollard/backup/{}",
            backup.file_name().unwrap_or_default().to_string_lossy()
        );
        db.execute(
            "UPDATE meta SET value=replace(value,'{backup}',?1) WHERE key='upgrade_notice'",
            [shown],
        )?;
        fs::rename(&legacy, &backup).at(&backup)?;
        for ext in ["-wal", "-shm"] {
            let _ = fs::remove_file(dot.join(format!("{LEGACY_DB}{ext}")));
        }
        abort_at("old_moved");
    }
    let notice: Option<String> = db
        .query_row(
            "SELECT value FROM meta WHERE key='upgrade_notice'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(n) = notice {
        eprint!("{n}");
        db.execute("DELETE FROM meta WHERE key='upgrade_notice'", [])?;
    }
    if !legacy.is_dir() {
        fs::create_dir(&legacy).at(&legacy)?;
        let txt = legacy.join("UPGRADED.txt");
        fs::write(&txt, UPGRADED_TXT).at(&txt)?;
    }
    Ok(())
}

/// Run the pending `MIGRATIONS` in one transaction. Returns their notice lines.
fn migrate(db: &Connection) -> Result<Vec<String>> {
    let v = user_version(db)?;
    if v > FORMAT {
        return Err(newer(v));
    }
    db.execute_batch("BEGIN IMMEDIATE")?;
    let res = (|| {
        let mut lines = vec![];
        for (i, m) in MIGRATIONS.iter().enumerate() {
            let target = i as i64 + 2;
            if v < target {
                lines.extend(m(db)?);
                db.pragma_update(None, "user_version", target)?;
            }
        }
        Ok(lines)
    })();
    match res {
        Ok(lines) => {
            db.execute_batch("COMMIT")?;
            Ok(lines)
        }
        Err(e) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Format 2: `pruned` becomes the `pruned_at` flag (the status goes back to the outcome
/// before the prune, read from the op log), pins get optional names, remote segments are
/// tracked, and `undo` stops at this point.
fn to_format2(db: &Connection) -> Result<Vec<String>> {
    db.execute_batch(
        "ALTER TABLE nodes ADD COLUMN pruned_at TEXT;
         CREATE TABLE pins2(node_id TEXT NOT NULL, name TEXT UNIQUE);
         INSERT INTO pins2(node_id, name) SELECT node_id, name FROM pins;
         DROP TABLE pins;
         ALTER TABLE pins2 RENAME TO pins;
         CREATE UNIQUE INDEX pins_unnamed ON pins(node_id) WHERE name IS NULL;
         CREATE TABLE remote_segments(name TEXT PRIMARY KEY, pins TEXT NOT NULL);
         DELETE FROM meta WHERE key='sync:pins';
         INSERT OR REPLACE INTO meta(key,value)
           SELECT 'migrated_at_op', COALESCE(MAX(op_id), 0) FROM ops;",
    )?;
    type Pruned = (String, Option<String>, String);
    let mut pruned: Vec<Pruned> = {
        let mut st =
            db.prepare("SELECT id, finished_at, created_at FROM nodes WHERE status='pruned' ORDER BY created_at, id")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    if pruned.is_empty() {
        return Ok(vec![]);
    }
    let total = pruned.len();
    // Newest op first: the first snapshot that shows a node unpruned is from its last prune.
    let mut st = db.prepare("SELECT ts, before_snapshot FROM ops ORDER BY op_id DESC")?;
    let mut rows = st.query([])?;
    while !pruned.is_empty() {
        let Some(r) = rows.next()? else { break };
        let (ts, blob): (String, Vec<u8>) = (r.get(0)?, r.get(1)?);
        let snap: serde_json::Value = match zstd::decode_all(&blob[..]) {
            Ok(raw) => serde_json::from_slice(&raw).unwrap_or_default(),
            Err(_) => continue,
        };
        let before: HashMap<&str, &str> = snap["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| Some((n["id"].as_str()?, n["status"].as_str()?)))
            .collect();
        let mut i = 0;
        while i < pruned.len() {
            match before.get(pruned[i].0.as_str()).copied() {
                Some(s) if s != "pruned" && node::Status::parse(s).is_some() => {
                    let (id, _, _) = pruned.remove(i);
                    db.execute(
                        "UPDATE nodes SET status=?1, pruned_at=?2 WHERE id=?3",
                        rusqlite::params![s, ts, id],
                    )?;
                }
                _ => i += 1,
            }
        }
    }
    let mut lines = vec![format!(
        "pruned is now a flag: {total} pruned nodes keep their done/failed/killed status"
    )];
    if !pruned.is_empty() {
        let mut guessed = vec![];
        for (id, finished, created) in &pruned {
            let status = if finished.is_some() { "done" } else { "killed" };
            db.execute(
                "UPDATE nodes SET status=?1, pruned_at=?2 WHERE id=?3",
                rusqlite::params![status, finished.as_deref().unwrap_or(created), id],
            )?;
            guessed.push(format!("{id} ({status})"));
        }
        lines.push(format!(
            "guessed status (no op-log record): {}",
            guessed.join(", ")
        ));
    }
    Ok(lines)
}

/// Exclusive lock on `.pollard/lock`; released on drop.
pub(crate) fn lock_at(dot: &Path) -> Result<Lock> {
    let p = dot.join("lock");
    let f = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&p)
        .at(&p)?;
    f.lock().at(&p)?;
    Ok(Lock(f))
}

/// Held while mutating; released on drop.
pub struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl Repo {
    pub fn init(root: &Path) -> Result<Repo> {
        let dot = root.join(".pollard");
        if dot.exists() {
            return Err(Error::AlreadyInit(dot));
        }
        fs::create_dir_all(&dot).at(&dot)?;
        fs::write(dot.join("config.toml"), config::DEFAULT_TOML).at(dot.join("config.toml"))?;
        // add .pollard/ to .gitignore
        let gi = root.join(".gitignore");
        let cur = fs::read_to_string(&gi).unwrap_or_default();
        if !cur
            .lines()
            .any(|l| l.trim() == ".pollard/" || l.trim() == ".pollard")
        {
            let sep = if cur.is_empty() || cur.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            fs::write(&gi, format!("{cur}{sep}.pollard/\n")).at(&gi)?;
        }
        let repo = Repo::open_at(root)?;
        repo.set_meta("salt", &crate::ids::new_salt(root))?;
        repo.set_meta("counter", "1")?;
        Ok(repo)
    }

    /// Find `.pollard/` in `start` or a parent.
    pub fn discover(start: &Path) -> Result<Repo> {
        let start = start.canonicalize().at(start)?;
        let mut cur = start.as_path();
        loop {
            if cur.join(".pollard").is_dir() {
                return Repo::open_at(cur);
            }
            cur = cur.parent().ok_or_else(|| Error::NotARepo(start.clone()))?;
        }
    }

    fn open_at(root: &Path) -> Result<Repo> {
        let root = root.canonicalize().at(root)?;
        let dot = root.join(".pollard");
        let cfg_path = dot.join("config.toml");
        let config = match fs::read_to_string(&cfg_path) {
            Ok(s) => Config::parse(&s).map_err(|e| msg(format!("{}: {e}", cfg_path.display())))?,
            Err(_) => Config::default(),
        };
        let db = open_db(&dot)?;
        let objects = pollard_objects::Store::new(&dot);
        Ok(Repo {
            root,
            dot,
            db,
            config,
            objects,
            cmdline: String::new(),
        })
    }

    /// Exclusive writer lock on `.pollard/lock`.
    pub fn lock(&self) -> Result<Lock> {
        lock_at(&self.dot)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }
    pub fn set_meta(&self, key: &str, v: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
            [key, v],
        )?;
        Ok(())
    }

    pub fn del_meta(&self, key: &str) -> Result<()> {
        self.db.execute("DELETE FROM meta WHERE key=?1", [key])?;
        Ok(())
    }

    /// Current node (`@`), if any.
    pub fn head(&self) -> Result<Option<String>> {
        self.meta("head")
    }
    pub fn set_head(&self, id: Option<&str>) -> Result<()> {
        match id {
            Some(id) => self.set_meta("head", id),
            None => self.del_meta("head"),
        }
    }

    /// Next unused node id; bumps the per-clone counter.
    pub fn next_id(&self) -> Result<String> {
        let salt = self.meta("salt")?.unwrap_or_default();
        let mut c: u64 = self
            .meta("counter")?
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        loop {
            let id = crate::ids::make(&salt, c);
            c += 1;
            if node::get(&self.db, &id)?.is_none() {
                self.set_meta("counter", &c.to_string())?;
                return Ok(id);
            }
        }
    }

    pub fn node(&self, id: &str) -> Result<node::Node> {
        node::get(&self.db, id)?.ok_or_else(|| Error::UnknownNode(id.into()))
    }

    /// Resolve a full id, `@`, `@-`, a pin name, or a unique prefix.
    pub fn resolve(&self, arg: &str) -> Result<String> {
        if arg == "@" || arg == "@-" {
            let head = self
                .head()?
                .ok_or_else(|| msg("no current node (run or fork first)"))?;
            if arg == "@" {
                return Ok(head);
            }
            return self
                .node(&head)?
                .parent
                .ok_or_else(|| msg(format!("{head} is a root; '@-' has no node")));
        }
        if node::get(&self.db, arg)?.is_some() {
            return Ok(arg.into());
        }
        if let Some(id) = self
            .db
            .query_row("SELECT node_id FROM pins WHERE name=?1", [arg], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
        {
            return Ok(id);
        }
        let mut st = self
            .db
            .prepare("SELECT id FROM nodes WHERE id LIKE ?1 ESCAPE '\\' LIMIT 6")?;
        let pat = format!(
            "{}%",
            arg.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let m: Vec<String> = st
            .query_map([pat], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        match m.len() {
            0 => Err(Error::UnknownNode(arg.into())),
            1 => Ok(m[0].clone()),
            _ => Err(Error::Ambiguous(arg.into(), m.join(", "))),
        }
    }

    /// Pin names pointing at `id`.
    pub fn pins_of(&self, id: &str) -> Result<Vec<String>> {
        let mut st = self
            .db
            .prepare("SELECT name FROM pins WHERE node_id=?1 AND name IS NOT NULL ORDER BY name")?;
        let v = st
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn snap(nodes: &[(&str, &str)]) -> Vec<u8> {
        let nodes: Vec<_> = nodes
            .iter()
            .map(|(id, st)| serde_json::json!({"id": id, "status": st}))
            .collect();
        let v = serde_json::json!({"head": null, "nodes": nodes, "pins": [["paper", "b"]]});
        zstd::encode_all(&serde_json::to_vec(&v).unwrap()[..], 3).unwrap()
    }

    /// A format-1 (0.1.0-alpha.1) `.pollard/db.sqlite`, left in WAL mode.
    fn format1(root: &Path) {
        let dot = root.join(".pollard");
        fs::create_dir_all(&dot).unwrap();
        let db = Connection::open(dot.join(LEGACY_DB)).unwrap();
        db.pragma_update(None, "journal_mode", "WAL").unwrap();
        db.execute_batch(SCHEMA).unwrap();
        // a: never pruned; b: pruned while failed; c/d: no op record (fallback);
        // r: pruned, undone, pruned again
        for (id, st, fin) in [
            ("a", "done", Some("F-a")),
            ("b", "pruned", Some("F-b")),
            ("c", "pruned", Some("F-c")),
            ("d", "pruned", None),
            ("r", "pruned", Some("F-r")),
        ] {
            db.execute(
                "INSERT INTO nodes(id,code,config,data,env,recipe_hash,status,created_at,finished_at,command,depth) VALUES(?1,'c','c','d','e',?1,?2,?3,?4,'x',0)",
                params![id, st, format!("C-{id}"), fin],
            )
            .unwrap();
        }
        db.execute("INSERT INTO pins(name,node_id) VALUES('paper','b')", [])
            .unwrap();
        for (ts, before) in [
            ("T1", snap(&[("b", "failed"), ("r", "done")])),
            ("T2", snap(&[("r", "pruned")])),
            ("T3", snap(&[("r", "done")])),
        ] {
            db.execute(
                "INSERT INTO ops(ts,command,before_snapshot,after_snapshot) VALUES(?1,'x',?2,?2)",
                params![ts, before],
            )
            .unwrap();
        }
    }

    fn row(r: &Repo, id: &str) -> (String, Option<String>) {
        r.db.query_row(
            "SELECT status, pruned_at FROM nodes WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn migrates_format1_once() {
        let t = tempfile::tempdir().unwrap();
        format1(t.path());
        let r = Repo::discover(t.path()).unwrap();
        assert_eq!(user_version(&r.db).unwrap(), FORMAT);
        assert_eq!(row(&r, "a"), ("done".into(), None));
        assert_eq!(row(&r, "b"), ("failed".into(), Some("T1".into())));
        assert_eq!(row(&r, "r"), ("done".into(), Some("T3".into())));
        assert_eq!(row(&r, "c"), ("done".into(), Some("F-c".into())));
        assert_eq!(row(&r, "d"), ("killed".into(), Some("C-d".into())));
        assert_eq!(r.resolve("paper").unwrap(), "b");
        assert_eq!(r.meta("migrated_at_op").unwrap().as_deref(), Some("3"));
        assert_eq!(r.meta("upgrade_notice").unwrap(), None);
        let dot = t.path().join(".pollard");
        assert!(dot.join("db.sqlite/UPGRADED.txt").is_file());
        for gone in ["db.sqlite-wal", "db.sqlite-shm", "state.sqlite.tmp"] {
            assert!(!dot.join(gone).exists(), "{gone}");
        }
        // the backup is standalone: a copy alone opens at format 1 with every row
        let copy = t.path().join("copy.sqlite");
        fs::copy(dot.join("backup/db-format1.sqlite"), &copy).unwrap();
        let b = Connection::open(&copy).unwrap();
        assert_eq!(user_version(&b).unwrap(), 0);
        let n: i64 = b
            .query_row(
                "SELECT count(*) FROM nodes WHERE status='pruned'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 4);
        drop(r);
        // second open: nothing to do
        Repo::discover(t.path()).unwrap();
        assert_eq!(fs::read_dir(dot.join("backup")).unwrap().count(), 1);

        // rollback by copy, re-migrate: the existing backup is kept, a new one is named
        fs::remove_dir_all(dot.join(LEGACY_DB)).unwrap();
        for f in ["state.sqlite", "state.sqlite-wal", "state.sqlite-shm"] {
            let _ = fs::remove_file(dot.join(f));
        }
        fs::copy(dot.join("backup/db-format1.sqlite"), dot.join(LEGACY_DB)).unwrap();
        let r = Repo::discover(t.path()).unwrap();
        assert_eq!(row(&r, "b"), ("failed".into(), Some("T1".into())));
        assert_eq!(fs::read_dir(dot.join("backup")).unwrap().count(), 2);
    }

    #[test]
    fn newer_format_refused_untouched() {
        let t = tempfile::tempdir().unwrap();
        Repo::init(t.path()).unwrap();
        let state = t.path().join(".pollard").join(DB);
        Connection::open(&state)
            .unwrap()
            .pragma_update(None, "user_version", 3)
            .unwrap();
        let before = fs::read(&state).unwrap();
        let e = Repo::discover(t.path()).err().unwrap().to_string();
        assert_eq!(
            e,
            format!(
                "this repo uses format 3, written by a newer pollard; this pollard ({}) reads up to format 2. Upgrade pollard.",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert_eq!(fs::read(&state).unwrap(), before);
    }
}
