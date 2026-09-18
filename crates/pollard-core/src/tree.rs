//! `pollard tree`: render the node tree with collapsing, sweeps as one row, best-path ★.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::node::{Node, Status};
use crate::siblings::fmt_num;
use crate::{Repo, Result, metrics, node};

#[derive(Debug, Default, Clone)]
pub struct Opts {
    pub metric: Option<String>,
    pub all: bool,
}

/// Higher is better for accuracy-like keys; lower otherwise (losses, errors, perplexity).
pub fn higher_is_better(key: &str) -> bool {
    let k = key.to_lowercase();
    [
        "acc",
        "score",
        "reward",
        "auc",
        "f1",
        "bleu",
        "precision",
        "recall",
        "map",
        "iou",
    ]
    .iter()
    .any(|s| k.contains(s))
}

pub fn render(repo: &Repo, o: &Opts) -> Result<String> {
    let nodes = node::all(&repo.db)?;
    let head = repo.head()?;
    let mut kids: HashMap<Option<String>, Vec<&Node>> = HashMap::new();
    for n in &nodes {
        kids.entry(n.parent.clone()).or_default().push(n);
    }
    let mut pins: HashMap<String, Vec<String>> = HashMap::new();
    {
        let mut st = repo
            .db
            .prepare("SELECT name, node_id FROM pins ORDER BY name")?;
        for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (name, id) = r?;
            pins.entry(id).or_default().push(name);
        }
    }
    // metric value (last own point) per node, and the best path
    let mut val: HashMap<String, f64> = HashMap::new();
    let mut best_path: HashSet<String> = HashSet::new();
    if let Some(k) = &o.metric {
        for n in &nodes {
            if let Some(&(_, v)) = metrics::series(&repo.db, &n.id, k)?.last() {
                val.insert(n.id.clone(), v);
            }
        }
        let hib = higher_is_better(k);
        let best = nodes
            .iter()
            .filter(|n| n.status != Status::Pruned || o.all)
            .filter_map(|n| val.get(&n.id).map(|v| (n, *v)))
            .max_by(|a, b| {
                if hib {
                    a.1.total_cmp(&b.1)
                } else {
                    b.1.total_cmp(&a.1)
                }
            });
        if let Some((b, _)) = best {
            best_path.extend(node::ancestry(&repo.db, &b.id)?.into_iter().map(|n| n.id));
        }
    }
    let ctx = Ctx {
        o,
        kids: &kids,
        pins: &pins,
        val: &val,
        best: &best_path,
        head: head.as_deref(),
    };
    let mut out = String::new();
    let roots = kids.get(&None).cloned().unwrap_or_default();
    ctx.children(&roots, "", true, &mut out);
    Ok(out)
}

struct Ctx<'a> {
    o: &'a Opts,
    kids: &'a HashMap<Option<String>, Vec<&'a Node>>,
    pins: &'a HashMap<String, Vec<String>>,
    val: &'a HashMap<String, f64>,
    best: &'a HashSet<String>,
    head: Option<&'a str>,
}

enum Item<'a> {
    Node(&'a Node),
    Sweep(String, Vec<&'a Node>),
}

impl<'a> Ctx<'a> {
    fn count(&self, id: &str) -> usize {
        self.kids
            .get(&Some(id.to_string()))
            .map_or(0, |v| v.iter().map(|k| 1 + self.count(&k.id)).sum())
    }

    fn children(&self, list: &[&'a Node], prefix: &str, top: bool, out: &mut String) {
        let mut items: Vec<Item> = vec![];
        let mut sweeps: BTreeMap<String, usize> = BTreeMap::new();
        for n in list {
            if n.status == Status::Pruned && !self.o.all {
                continue;
            }
            match &n.sweep {
                Some(s) => match sweeps.get(s) {
                    Some(&i) => {
                        if let Item::Sweep(_, v) = &mut items[i] {
                            v.push(n)
                        }
                    }
                    None => {
                        sweeps.insert(s.clone(), items.len());
                        items.push(Item::Sweep(s.clone(), vec![n]));
                    }
                },
                None => items.push(Item::Node(n)),
            }
        }
        let len = items.len();
        for (i, it) in items.into_iter().enumerate() {
            let last = i + 1 == len;
            let (branch, cont) = if top {
                ("", "")
            } else if last {
                ("└─ ", "   ")
            } else {
                ("├─ ", "│  ")
            };
            match it {
                Item::Sweep(name, members) => {
                    let mut line = format!("{prefix}{branch}sweep:{name}  {} runs", members.len());
                    let vals: Vec<f64> = members
                        .iter()
                        .filter_map(|m| self.val.get(&m.id).copied())
                        .collect();
                    if !vals.is_empty() {
                        let n = vals.len() as f64;
                        let mean = vals.iter().sum::<f64>() / n;
                        let sd = if vals.len() > 1 {
                            (vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0))
                                .sqrt()
                        } else {
                            0.0
                        };
                        line.push_str(&format!("  {} ± {}", fmt_num(mean), fmt_num(sd)));
                    }
                    if members.iter().any(|m| self.best.contains(&m.id)) {
                        line.push_str(" ★");
                    }
                    if members.iter().any(|m| Some(m.id.as_str()) == self.head) {
                        line.push_str("  (@)");
                    }
                    out.push_str(&line);
                    out.push('\n');
                }
                Item::Node(n) => {
                    out.push_str(&format!("{prefix}{branch}{}\n", self.line(n)));
                    let sub = self
                        .kids
                        .get(&Some(n.id.clone()))
                        .cloned()
                        .unwrap_or_default();
                    let collapsed = n.status == Status::Failed && !self.o.all && !sub.is_empty();
                    if collapsed {
                        out.push_str(&format!(
                            "{prefix}{cont}└─ … {} collapsed\n",
                            self.count(&n.id)
                        ));
                    } else {
                        self.children(&sub, &format!("{prefix}{cont}"), false, out);
                    }
                }
            }
        }
    }

    fn line(&self, n: &Node) -> String {
        let mut s = n.id.clone();
        if let Some(fs) = n.fork_step {
            s.push_str(&format!(" @{fs}"));
        }
        if let Some(p) = self.pins.get(&n.id) {
            s.push_str(&format!(" [{}]", p.join(", ")));
        }
        if Some(n.id.as_str()) == self.head {
            s.push_str(" (@)");
        }
        s.push_str(&format!("  {}", n.status.as_str()));
        if let Some(v) = self.val.get(&n.id) {
            s.push_str(&format!("  {}", fmt_num(*v)));
        }
        if self.best.contains(&n.id) {
            s.push_str(" ★");
        }
        let t = n.title();
        if !t.is_empty() {
            s.push_str(&format!(
                "  {}{t}",
                if n.note_auto { "(auto) " } else { "" }
            ));
        }
        s
    }
}
