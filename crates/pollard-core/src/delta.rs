//! Parent-relative deltas (§6 step 1), computed once at node creation and stored in `deltas`.

use pollard_objects::{Change, ChangeKind};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Repo, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigChange {
    pub path: String,
    pub old: Value,
    pub new: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DataDelta {
    pub changes: Vec<Change>,
    pub files_added: u64,
    pub files_removed: u64,
    pub bytes_delta: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvChange {
    pub name: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Deltas {
    pub config: Vec<ConfigChange>,
    pub code: Vec<Change>,
    pub data: DataDelta,
    pub env: Vec<EnvChange>,
}

/// Structural diff with dotted paths; arrays by index; one-sided keys get `null`.
pub fn config_delta(old: &Value, new: &Value) -> Vec<ConfigChange> {
    let mut out = vec![];
    walk("", old, new, &mut out);
    out
}

fn walk(prefix: &str, a: &Value, b: &Value, out: &mut Vec<ConfigChange>) {
    let join = |k: &str| {
        if prefix.is_empty() {
            k.to_string()
        } else {
            format!("{prefix}.{k}")
        }
    };
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for k in keys {
                walk(
                    &join(k),
                    x.get(k).unwrap_or(&Value::Null),
                    y.get(k).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            for i in 0..x.len().max(y.len()) {
                walk(
                    &join(&i.to_string()),
                    x.get(i).unwrap_or(&Value::Null),
                    y.get(i).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        _ if a != b => out.push(ConfigChange {
            path: prefix.to_string(),
            old: a.clone(),
            new: b.clone(),
        }),
        _ => {}
    }
}

/// Compact scalar rendering: strings unquoted, floats in the shorter of plain/exponent form.
pub fn fmt_value(v: &Value) -> String {
    match v {
        Value::Null => "∅".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_f64() => fmt_f64(n.as_f64().unwrap()),
        other => other.to_string(),
    }
}

pub fn fmt_f64(f: f64) -> String {
    let (plain, exp) = (format!("{f}"), format!("{f:e}"));
    if exp.len() < plain.len() { exp } else { plain }
}

pub fn fmt_change(c: &ConfigChange) -> String {
    format!("{}→{}", fmt_value(&c.old), fmt_value(&c.new))
}

/// `+attn.py`, `−2 files`, `~model.py +attn.py`.
pub fn summarize_changes(ch: &[Change]) -> String {
    let sign = |k: ChangeKind| match k {
        ChangeKind::Added => "+",
        ChangeKind::Removed => "−",
        ChangeKind::Modified => "~",
    };
    if ch.len() <= 2 {
        return ch
            .iter()
            .map(|c| format!("{}{}", sign(c.kind), c.path))
            .collect::<Vec<_>>()
            .join(" ");
    }
    [ChangeKind::Added, ChangeKind::Removed, ChangeKind::Modified]
        .into_iter()
        .filter_map(|k| {
            let n = ch.iter().filter(|c| c.kind == k).count();
            (n > 0).then(|| format!("{}{n} file{}", sign(k), if n == 1 { "" } else { "s" }))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn summarize_env(ch: &[EnvChange]) -> String {
    let one = |c: &EnvChange| match (&c.old, &c.new) {
        (Some(o), Some(n)) => format!("{} {o}→{n}", c.name),
        (None, Some(n)) => format!("+{} {n}", c.name),
        (Some(o), None) => format!("−{} {o}", c.name),
        (None, None) => c.name.clone(),
    };
    if ch.len() <= 2 {
        ch.iter().map(one).collect::<Vec<_>>().join(", ")
    } else {
        format!("{} packages", ch.len())
    }
}

pub fn summarize_data(d: &DataDelta) -> String {
    if d.changes.is_empty() {
        return String::new();
    }
    if d.changes.len() <= 2 {
        return summarize_changes(&d.changes);
    }
    let mut parts = vec![];
    if d.files_added > 0 {
        parts.push(format!("+{} files", d.files_added));
    }
    if d.files_removed > 0 {
        parts.push(format!("−{} files", d.files_removed));
    }
    let modified = d.changes.len() as u64 - d.files_added - d.files_removed;
    if modified > 0 {
        parts.push(format!("~{modified} files"));
    }
    parts.join(" ")
}

/// Code changes minus the captured config file (it shows as config rows instead).
pub fn visible_code(d: &Deltas, config_file: Option<&str>) -> Vec<Change> {
    d.code
        .iter()
        .filter(|c| Some(c.path.as_str()) != config_file)
        .cloned()
        .collect()
}

/// Auto note (§3): the config delta, else the code delta, else "rerun".
pub fn auto_note(d: &Deltas, config_file: Option<&str>) -> String {
    if !d.config.is_empty() {
        return d
            .config
            .iter()
            .map(|c| format!("{} {}", c.path, fmt_change(c)))
            .collect::<Vec<_>>()
            .join(", ");
    }
    let code = visible_code(d, config_file);
    if !code.is_empty() {
        return summarize_changes(&code);
    }
    "rerun".into()
}

pub fn store(repo: &Repo, node: &str, d: &Deltas) -> Result<()> {
    repo.db.execute(
        "INSERT OR REPLACE INTO deltas(node_id,config_delta,code_delta,data_delta,env_delta) VALUES(?1,?2,?3,?4,?5)",
        params![
            node,
            serde_json::to_string(&d.config)?,
            serde_json::to_string(&d.code)?,
            serde_json::to_string(&d.data)?,
            serde_json::to_string(&d.env)?
        ],
    )?;
    Ok(())
}

pub fn load(repo: &Repo, node: &str) -> Result<Deltas> {
    let row: Option<(String, String, String, String)> = repo
        .db
        .query_row(
            "SELECT config_delta,code_delta,data_delta,env_delta FROM deltas WHERE node_id=?1",
            [node],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((c, k, d, e)) = row else {
        return Ok(Deltas::default());
    };
    Ok(Deltas {
        config: serde_json::from_str(&c)?,
        code: serde_json::from_str(&k)?,
        data: serde_json::from_str(&d)?,
        env: serde_json::from_str(&e)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_paths() {
        let a =
            json!({"lr": 3e-4, "model": {"depth": 12, "heads": 8}, "layers": [1, 2], "gone": 1});
        let b = json!({"lr": 1e-4, "model": {"depth": 24, "heads": 8}, "layers": [1, 3, 4], "new": "x"});
        let d = config_delta(&a, &b);
        let got: Vec<String> = d
            .iter()
            .map(|c| format!("{} {}", c.path, fmt_change(c)))
            .collect();
        assert_eq!(
            got,
            [
                "gone 1→∅",
                "layers.1 2→3",
                "layers.2 ∅→4",
                "lr 3e-4→1e-4",
                "model.depth 12→24",
                "new ∅→x"
            ]
        );
    }

    #[test]
    fn summaries() {
        let c = |p: &str, k| Change {
            path: p.into(),
            kind: k,
        };
        assert_eq!(
            summarize_changes(&[c("attn.py", ChangeKind::Added)]),
            "+attn.py"
        );
        let three = [
            c("a", ChangeKind::Removed),
            c("b", ChangeKind::Removed),
            c("d", ChangeKind::Added),
        ];
        assert_eq!(summarize_changes(&three), "+1 file −2 files");
        assert_eq!(fmt_f64(0.1), "0.1");
        assert_eq!(fmt_f64(12.0), "12");
    }
}
