//! Sibling table (§6 steps 2–4): a join over cached parent-relative deltas.

use std::collections::BTreeMap;

use crate::delta::{self, Deltas};
use crate::node::{Node, Status};
use crate::{Repo, Result, metrics, node};

#[derive(Debug, Default, Clone)]
pub struct Opts {
    pub metric: Option<String>,
    pub expand_sweeps: bool,
    pub all: bool,
    /// Restrict to these children (used by `diff` of two siblings); disables grouping.
    pub only: Option<Vec<String>>,
}

/// One column: a single child, a sweep, or a seed group.
#[derive(Debug, Clone)]
pub struct Column {
    pub header: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub group: &'static str,
    pub label: String,
    pub cells: Vec<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct Table {
    pub parent: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
}

struct Member {
    node: Node,
    d: Deltas,
}

fn is_seed_path(p: &str, seed_keys: &[String]) -> bool {
    seed_keys.iter().any(|k| p == k || p.ends_with(&format!(".{k}")))
}

/// Deltas with seed paths removed, as a grouping key.
fn seedless_key(d: &Deltas, seed_keys: &[String], cfg_file: Option<&str>) -> String {
    let cfg: Vec<_> = d.config.iter().filter(|c| !is_seed_path(&c.path, seed_keys)).collect();
    serde_json::json!([cfg, delta::visible_code(d, cfg_file), d.data, d.env]).to_string()
}

pub fn build(repo: &Repo, parent: &str, o: &Opts) -> Result<Table> {
    let seed_keys = &repo.config.seed_keys;
    let kids: Vec<Member> = node::children(&repo.db, parent)?
        .into_iter()
        .filter(|n| o.all || n.status != Status::Pruned)
        .filter(|n| o.only.as_ref().is_none_or(|v| v.contains(&n.id)))
        .map(|n| Ok(Member { d: delta::load(repo, &n.id)?, node: n }))
        .collect::<Result<_>>()?;

    // Columns: sweeps collapse, then seed groups, else one per child (created_at order).
    let mut groups: Vec<(String, Vec<usize>)> = vec![];
    let mut by_key: BTreeMap<String, usize> = BTreeMap::new();
    for (i, m) in kids.iter().enumerate() {
        let key = match (&m.node.sweep, o.expand_sweeps || o.only.is_some()) {
            (Some(s), false) => Some(format!("sweep:{s}")),
            _ if o.only.is_some() => None,
            (Some(_), true) => None, // --expand-sweeps: members stand alone
            _ if m.d.config.iter().any(|c| is_seed_path(&c.path, seed_keys)) => {
                Some(format!("seeds:{}", seedless_key(&m.d, seed_keys, crate::wc::config_file(repo).as_deref())))
            }
            _ => None,
        };
        match key.as_ref().and_then(|k| by_key.get(k)) {
            Some(&g) => groups[g].1.push(i),
            None => {
                if let Some(k) = key {
                    by_key.insert(k, groups.len());
                }
                groups.push((String::new(), vec![i]));
            }
        }
    }
    let mut columns = vec![];
    for (_, idx) in &mut groups {
        let first = &kids[idx[0]].node;
        let header = if idx.len() == 1 {
            let pins = repo.pins_of(&first.id)?;
            pins.into_iter().next().unwrap_or_else(|| first.id.clone())
        } else if let (Some(s), false) = (&first.sweep, o.expand_sweeps || o.only.is_some()) {
            format!("sweep:{s} ({})", idx.len())
        } else {
            format!("{} seeds", idx.len())
        };
        columns.push(Column { header, members: idx.iter().map(|&i| kids[i].node.id.clone()).collect() });
    }

    let cell = |idx: &[usize], f: &dyn Fn(&Member) -> Option<String>| -> Option<String> {
        let vals: Vec<Option<String>> = idx.iter().map(|&i| f(&kids[i])).collect();
        if vals.iter().all(|v| *v == vals[0]) {
            return vals[0].clone();
        }
        let distinct: std::collections::BTreeSet<_> = vals.iter().collect();
        Some(format!("{} values", distinct.len()))
    };

    let mut rows = vec![];
    // config rows: union of paths, first-seen order sorted by path
    let paths: std::collections::BTreeSet<String> =
        kids.iter().flat_map(|m| m.d.config.iter().map(|c| c.path.clone())).collect();
    for p in paths {
        let cells = groups
            .iter()
            .map(|(_, idx)| {
                cell(idx, &|m| m.d.config.iter().find(|c| c.path == p).map(delta::fmt_change))
            })
            .collect();
        rows.push(Row { group: "config", label: p, cells });
    }
    let cfg_file = crate::wc::config_file(repo);
    let summaries: [(&'static str, &dyn Fn(&Member) -> String); 3] = [
        ("code", &|m| delta::summarize_changes(&delta::visible_code(&m.d, cfg_file.as_deref()))),
        ("data", &|m| delta::summarize_data(&m.d.data)),
        ("env", &|m| delta::summarize_env(&m.d.env)),
    ];
    for (label, f) in summaries {
        let cells =
            groups.iter().map(|(_, idx)| cell(idx, &|m| Some(f(m)).filter(|s| !s.is_empty()))).collect();
        rows.push(Row { group: label, label: label.into(), cells });
    }

    // metrics
    let key = match &o.metric {
        Some(k) => Some(k.clone()),
        None => match &repo.config.primary_metric {
            Some(k) => Some(k.clone()),
            None => {
                let mut ks = std::collections::BTreeSet::new();
                for m in &kids {
                    ks.extend(metrics::keys(&repo.db, &m.node.id)?);
                }
                ks.into_iter().next()
            }
        },
    };
    if let Some(key) = key {
        rows.extend(metric_rows(repo, parent, &key, &kids, &groups)?);
    }

    let cells = groups
        .iter()
        .map(|(_, idx)| {
            if idx.len() == 1 {
                return Some(kids[idx[0]].node.status.as_str().to_string());
            }
            let mut c: BTreeMap<&str, usize> = BTreeMap::new();
            for &i in idx {
                *c.entry(kids[i].node.status.as_str()).or_default() += 1;
            }
            Some(c.iter().map(|(s, n)| format!("{n} {s}")).collect::<Vec<_>>().join(" "))
        })
        .collect();
    rows.push(Row { group: "status", label: "status".into(), cells });

    rows.retain(|r| r.cells.iter().any(|c| c.is_some()));
    Ok(Table { parent: parent.to_string(), columns, rows })
}

fn metric_rows(
    repo: &Repo,
    parent: &str,
    key: &str,
    kids: &[Member],
    groups: &[(String, Vec<usize>)],
) -> Result<Vec<Row>> {
    let series: Vec<Vec<(i64, f64)>> =
        kids.iter().map(|m| metrics::series(&repo.db, &m.node.id, key)).collect::<Result<_>>()?;
    let pseries = metrics::series(&repo.db, parent, key)?;
    let plast = pseries.last().map(|p| p.0);
    let at = |step: i64| plast.filter(|l| *l >= step).and_then(|_| metrics::value_at(&pseries, step));
    // last_common: largest step every non-failed child with points reached
    let last_common = kids
        .iter()
        .zip(&series)
        .filter(|(m, s)| m.node.status != Status::Failed && !s.is_empty())
        .map(|(_, s)| s.last().unwrap().0)
        .min();
    let mut rows = vec![];
    let fmt_group = |vals: Vec<(f64, Option<f64>)>| -> Option<String> {
        if vals.is_empty() {
            return None;
        }
        if vals.len() == 1 {
            let (v, p) = vals[0];
            return Some(fmt_with_delta(v, p));
        }
        let n = vals.len() as f64;
        let mean = vals.iter().map(|v| v.0).sum::<f64>() / n;
        let std = (vals.iter().map(|v| (v.0 - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
        Some(format!("{} ± {}", fmt_num(mean), fmt_num(std)))
    };
    if let Some(lc) = last_common {
        let cells = groups
            .iter()
            .map(|(_, idx)| {
                fmt_group(
                    idx.iter()
                        .filter_map(|&i| metrics::value_at(&series[i], lc).map(|v| (v, at(lc))))
                        .collect(),
                )
            })
            .collect();
        rows.push(Row { group: "metrics", label: format!("{key}@{}", fmt_step(lc)), cells });
    }
    let cells = groups
        .iter()
        .map(|(_, idx)| {
            let vals: Vec<(f64, Option<f64>)> =
                idx.iter().filter_map(|&i| series[i].last().map(|&(s, v)| (v, at(s)))).collect();
            let c = fmt_group(vals)?;
            Some(match idx.as_slice() {
                [i] => format!("{c}@{}", fmt_step(series[*i].last()?.0)),
                _ => c,
            })
        })
        .collect();
    rows.push(Row { group: "metrics", label: format!("{key}@last"), cells });
    Ok(rows)
}

/// Up to 4 significant digits, trailing zeros trimmed.
pub fn fmt_num(v: f64) -> String {
    if v == 0.0 || !v.is_finite() {
        return format!("{v}");
    }
    let mag = v.abs().log10().floor() as i32;
    if !(-4..6).contains(&mag) {
        return format!("{v:.3e}");
    }
    let decimals = (3 - mag).max(0) as usize;
    let s = format!("{v:.decimals$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

/// `2.19(−.16)`; value alone without a parent value.
fn fmt_with_delta(v: f64, parent: Option<f64>) -> String {
    match parent {
        None => fmt_num(v),
        Some(p) => {
            let d = v - p;
            let sign = if d < 0.0 { "−" } else { "+" };
            let mag = fmt_num(d.abs());
            let mag = mag.strip_prefix("0.").map_or(mag.clone(), |r| format!(".{r}"));
            format!("{}({sign}{mag})", fmt_num(v))
        }
    }
}

pub fn fmt_step(s: i64) -> String {
    if s >= 1_000_000 && s % 1_000_000 == 0 {
        format!("{}M", s / 1_000_000)
    } else if s >= 1000 && s % 1000 == 0 {
        format!("{}k", s / 1000)
    } else {
        s.to_string()
    }
}

fn w(s: &str) -> usize {
    s.chars().count()
}

fn pad(s: &str, n: usize) -> String {
    format!("{s}{}", " ".repeat(n.saturating_sub(w(s))))
}

/// Fixed-width table; more than 6 columns are transposed (one child per row).
pub fn render(t: &Table) -> String {
    let blank = "—";
    let mut grid: Vec<Vec<String>> = vec![];
    if t.columns.len() <= 6 {
        let mut head = vec![String::new()];
        head.extend(t.columns.iter().map(|c| c.header.clone()));
        grid.push(head);
        for r in &t.rows {
            let mut line = vec![r.label.clone()];
            line.extend(r.cells.iter().map(|c| c.clone().unwrap_or_else(|| blank.into())));
            grid.push(line);
        }
    } else {
        let mut head = vec![String::new()];
        head.extend(t.rows.iter().map(|r| r.label.clone()));
        grid.push(head);
        for (i, c) in t.columns.iter().enumerate() {
            let mut line = vec![c.header.clone()];
            line.extend(t.rows.iter().map(|r| r.cells[i].clone().unwrap_or_else(|| blank.into())));
            grid.push(line);
        }
    }
    let ncol = grid[0].len();
    let widths: Vec<usize> = (0..ncol).map(|j| grid.iter().map(|l| w(&l[j])).max().unwrap_or(0)).collect();
    let mut out = String::new();
    for l in &grid {
        let s: Vec<String> = l.iter().enumerate().map(|(j, c)| pad(c, widths[j] + 2)).collect();
        out.push_str(s.concat().trim_end());
        out.push('\n');
    }
    out
}

/// `{"<column header>": {"<row label>": cell | null, ...}, ...}` in display order:
/// the shape pandas reads with `pd.read_json` / `pd.DataFrame(...)`. Stable from M2.
pub fn to_json(t: &Table) -> String {
    let q = |s: &str| serde_json::to_string(s).unwrap();
    let cols: Vec<String> = t
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let cells: Vec<String> = t
                .rows
                .iter()
                .map(|r| format!("{}: {}", q(&r.label), r.cells[i].as_deref().map_or("null".into(), q)))
                .collect();
            format!("  {}: {{{}}}", q(&c.header), cells.join(", "))
        })
        .collect();
    format!("{{\n{}\n}}", cols.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formats() {
        assert_eq!(fmt_num(2.19), "2.19");
        assert_eq!(fmt_num(2.3100001), "2.31");
        assert_eq!(fmt_with_delta(2.19, Some(2.35)), "2.19(−.16)");
        assert_eq!(fmt_with_delta(2.19, None), "2.19");
        assert_eq!(fmt_step(10_000), "10k");
        assert_eq!(fmt_step(150), "150");
    }
}
