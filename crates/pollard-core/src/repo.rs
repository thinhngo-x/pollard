//! Repository handle: `.pollard/` layout, SQLite (WAL), file lock, node resolution.

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

/// Held while mutating; released on drop.
pub struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs4::fs_std::FileExt::unlock(&self.0);
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
        if !cur.lines().any(|l| l.trim() == ".pollard/" || l.trim() == ".pollard") {
            let sep = if cur.is_empty() || cur.ends_with('\n') { "" } else { "\n" };
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
        let db = Connection::open(dot.join("db.sqlite"))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.busy_timeout(std::time::Duration::from_secs(30))?;
        db.execute_batch(SCHEMA)?;
        let objects = pollard_objects::Store::new(&dot);
        Ok(Repo { root, dot, db, config, objects, cmdline: String::new() })
    }

    /// Exclusive writer lock on `.pollard/lock`.
    pub fn lock(&self) -> Result<Lock> {
        let p = self.dot.join("lock");
        let f = File::options().create(true).truncate(false).write(true).open(&p).at(&p)?;
        fs4::fs_std::FileExt::lock_exclusive(&f).at(&p)?;
        Ok(Lock(f))
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional()?)
    }
    pub fn set_meta(&self, key: &str, v: &str) -> Result<()> {
        self.db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)", [key, v])?;
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
        let mut c: u64 = self.meta("counter")?.and_then(|s| s.parse().ok()).unwrap_or(1);
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
            let head = self.head()?.ok_or_else(|| msg("no current node (run or fork first)"))?;
            if arg == "@" {
                return Ok(head);
            }
            return self.node(&head)?.parent.ok_or_else(|| msg(format!("{head} is a root; '@-' has no node")));
        }
        if node::get(&self.db, arg)?.is_some() {
            return Ok(arg.into());
        }
        if let Some(id) =
            self.db.query_row("SELECT node_id FROM pins WHERE name=?1", [arg], |r| r.get::<_, String>(0)).optional()?
        {
            return Ok(id);
        }
        let mut st = self.db.prepare("SELECT id FROM nodes WHERE id LIKE ?1 ESCAPE '\\' LIMIT 6")?;
        let pat = format!("{}%", arg.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let m: Vec<String> = st.query_map([pat], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        match m.len() {
            0 => Err(Error::UnknownNode(arg.into())),
            1 => Ok(m[0].clone()),
            _ => Err(Error::Ambiguous(arg.into(), m.join(", "))),
        }
    }

    /// Pin names pointing at `id`.
    pub fn pins_of(&self, id: &str) -> Result<Vec<String>> {
        let mut st = self.db.prepare("SELECT name FROM pins WHERE node_id=?1 ORDER BY name")?;
        let v = st.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }
}
