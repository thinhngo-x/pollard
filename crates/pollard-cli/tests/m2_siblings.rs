//! M2: deltas, `diff`, `siblings` with config/code rows, off-tree files, `output_dirs`.
mod common;
use common::*;
use std::collections::BTreeMap;

/// Parse a rendered sibling table into (header ids, label → cells).
/// Accepts `│`/`|`-bordered tables (empty cells kept) or whitespace tables (cells split on 2+
/// spaces; blanks must then be a placeholder like `—`). Cells are normalised: `->`→`→`, `−`→`-`,
/// quotes stripped, spaces around `→` removed, `—`/`–`/`-` alone → "".
pub fn parse_table(text: &str, ids: &[String]) -> (Vec<String>, BTreeMap<String, Vec<String>>) {
    let norm = |c: &str| {
        let c = c
            .trim()
            .replace("->", "→")
            .replace('−', "-")
            .replace(['"', '\''], "")
            .replace(" → ", "→")
            .replace("→ ", "→")
            .replace(" →", "→");
        if matches!(c.as_str(), "—" | "–" | "-" | "") {
            String::new()
        } else {
            c
        }
    };
    let bordered = text.contains('│') || text.contains('|');
    let split = |l: &str| -> Vec<String> {
        if bordered {
            let l = l.trim();
            let l = l
                .trim_start_matches(['│', '|', '┃'])
                .trim_end_matches(['│', '|', '┃']);
            l.split(['│', '|', '┃']).map(norm).collect()
        } else {
            let mut cells = vec![];
            let mut cur = String::new();
            let mut spaces = 0;
            for ch in l.trim_end().chars() {
                if ch == ' ' {
                    spaces += 1;
                    continue;
                }
                if spaces >= 2 && !cur.is_empty() {
                    cells.push(norm(&cur));
                    cur.clear();
                } else if spaces == 1 {
                    cur.push(' ');
                }
                spaces = 0;
                cur.push(ch);
            }
            cells.push(norm(&cur));
            if l.starts_with("  ") {
                cells.insert(0, String::new());
            }
            cells
        }
    };
    let mut header = vec![];
    let mut rows = BTreeMap::new();
    for l in text.lines() {
        if l.trim().is_empty() || l.chars().all(|c| "─━┼┬┴├┤┌┐└┘+-=╞╪╡ │|".contains(c))
        {
            continue;
        }
        let cells = split(l);
        if header.is_empty() {
            if ids.iter().all(|id| l.contains(id.as_str())) {
                header = cells.into_iter().filter(|c| !c.is_empty()).collect();
            }
            continue;
        }
        let label = cells[0].clone();
        rows.insert(label, cells[1..].to_vec());
    }
    (header, rows)
}

fn five_children() -> (Repo, String, Vec<String>) {
    let r = Repo::init();
    r.write("util.py", "def u(): pass\n");
    r.write("NOTES.md", "# ideas\n");
    let p = r.run(&["-m", "parent"], "true");
    let base_cfg = "lr: 3\ndepth: 12\nact: relu\n";
    let mut kids = vec![];
    let mut child = |cfg: &str, add_attn: bool, rm_util: bool, note: &str| {
        r.write("config.yaml", cfg);
        if add_attn {
            r.write("attn.py", "class Attn: pass\n")
        } else if r.root.join("attn.py").exists() {
            r.rm("attn.py")
        }
        if rm_util {
            if r.root.join("util.py").exists() {
                r.rm("util.py")
            }
        } else {
            r.write("util.py", "def u(): pass\n")
        }
        kids.push(r.run(&["-m", note, "--parent", &p], "true"));
    };
    child("lr: 3\ndepth: 24\nact: relu\n", false, false, "c1 deeper");
    child("lr: 3\ndepth: 12\nact: gelu\n", false, false, "c2 gelu");
    child(base_cfg, true, false, "c3 attn");
    child(
        "lr: 3\ndepth: 24\nact: relu\n",
        true,
        false,
        "c4 deeper+attn",
    );
    child(base_cfg, false, true, "c5 no util");
    (r, p, kids)
}

/// §9 M2: "Parent with 5 children: sibling table matches a hand-written expected table;
/// blank rows are dropped".
#[test]
fn sibling_table_matches_hand_written_expected_table() {
    let (r, p, kids) = five_children();
    let out = r.ok(&["siblings", &p]).stdout;
    let (header, rows) = parse_table(&out, &kids);
    assert_eq!(
        header, kids,
        "columns must be one per child ordered by created_at:\n{out}"
    );
    let e = |v: [&str; 5]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut expected = BTreeMap::new();
    expected.insert("depth".to_string(), e(["12→24", "", "", "12→24", ""]));
    expected.insert("act".to_string(), e(["", "relu→gelu", "", "", ""]));
    expected.insert(
        "code".to_string(),
        e(["", "", "+attn.py", "+attn.py", "-util.py"]),
    );
    expected.insert("status".to_string(), e(["done"; 5]));
    // Row set must match exactly: `lr`, data, env and metric rows are blank for every child → dropped.
    assert_eq!(rows, expected, "sibling table mismatch. raw output:\n{out}");
    // Grouping order: config rows, then code, …, status last.
    let pos = |k: &str| {
        out.lines()
            .position(|l| l.trim_start_matches(['│', '|', ' ']).starts_with(k))
            .unwrap()
    };
    assert!(
        pos("depth") < pos("code") && pos("act") < pos("code") && pos("code") < pos("status"),
        "row grouping order wrong:\n{out}"
    );
}

#[test]
fn siblings_default_is_current_nodes_parent() {
    let (r, p, _kids) = five_children();
    let a = r.ok(&["siblings"]).stdout;
    let b = r.ok(&["siblings", &p]).stdout;
    assert_eq!(
        a, b,
        "`siblings` with no arg should equal `siblings <parent of @>`"
    );
}

#[test]
fn sib_short_form() {
    let (r, p, _) = five_children();
    assert_eq!(r.ok(&["sib", &p]).stdout, r.ok(&["siblings", &p]).stdout);
}

/// §9 M2: "`--json` round-trips to a DataFrame".
#[test]
fn siblings_json_round_trips_to_dataframe() {
    let (r, p, kids) = five_children();
    let js = r.ok(&["siblings", &p, "--json"]).stdout;
    std::fs::write(r.aux.join("sib.json"), &js).unwrap();
    let py = format!(
        r#"
import json, io, pandas as pd
raw = open({path:?}).read()
data = json.loads(raw)
df = pd.read_json(io.StringIO(raw)) if not isinstance(data, dict) or all(isinstance(v,(list,dict)) for v in data.values()) else pd.DataFrame(data)
back = pd.read_json(io.StringIO(df.to_json()))
assert back.shape == df.shape, (back.shape, df.shape)
blob = df.to_string() + " ".join(map(str, df.columns)) + " ".join(map(str, df.index))
missing = [k for k in {kids:?} if k not in blob]
assert not missing, ("children missing from DataFrame", missing, df)
print("OK", df.shape)
"#,
        path = r.aux.join("sib.json").to_string_lossy(),
        kids = kids
    );
    std::fs::write(r.aux.join("rt.py"), py).unwrap();
    let o = r.exec(r.cmd_in(
        &r.aux,
        std::path::Path::new("uv"),
        &[
            "run",
            "--no-project",
            "--quiet",
            "--with",
            "pandas",
            "python",
            "rt.py",
        ],
    ));
    assert!(
        o.ok() && o.stdout.contains("OK"),
        "siblings --json did not load into a DataFrame:\njson:\n{js}\n{o}"
    );
}

/// §9 M2: "editing an off-tree `.md` between runs produces no code delta and no duplicate-check bypass".
#[test]
fn offtree_md_edit_is_not_a_code_delta_and_does_not_bypass_duplicate_check() {
    let r = Repo::init();
    r.write("REPORT.md", "v1\n");
    r.write("notes/idea.txt", "v1\n");
    let a = r.run(&["-m", "a"], "true");
    r.write("REPORT.md", "v2 edited\n");
    r.write("notes/idea.txt", "v2\n");
    let o = r.run_out(&["-m", "only docs changed"], "true");
    assert!(!o.ok(), "off-tree edit bypassed the duplicate check:\n{o}");
    let b = r.run(&["-m", "forced", "--force"], "true");
    let d = r.ok(&["diff", &a, &b]).stdout;
    assert!(
        !d.contains("REPORT.md") && !d.contains("idea.txt"),
        "off-tree file shows up in default diff:\n{d}"
    );
    let s = r.ok(&["siblings", &a]).stdout;
    assert!(
        !s.contains("REPORT")
            && !s
                .lines()
                .any(|l| l.trim_start_matches(['│', '|', ' ']).starts_with("code")),
        "off-tree edit produced a code row:\n{s}"
    );
    let dd = r.ok(&["diff", &a, &b, "--docs"]).stdout;
    assert!(
        dd.contains("REPORT.md"),
        "`diff --docs` does not show the off-tree change:\n{dd}"
    );
}

#[test]
fn show_displays_offtree_snapshot_at_run_time() {
    let r = Repo::init();
    r.write("REPORT.md", "report said THIS-AT-RUN-TIME\n");
    let a = r.run(&["-m", "a"], "true");
    r.write("REPORT.md", "changed later\n");
    let s = r.ok(&["show", &a]).stdout;
    assert!(
        s.contains("REPORT.md"),
        "show does not list the docs snapshot (§3 off-tree):\n{s}"
    );
}

/// §3: outputs written to `output_dirs` (default `outputs/`, auto-ignored) are outcomes, not code.
#[test]
fn output_dirs_are_not_part_of_the_recipe() {
    let r = Repo::init();
    r.run(
        &["-m", "a"],
        "mkdir -p outputs; echo result > outputs/result.txt",
    );
    assert!(r.root.join("outputs/result.txt").exists());
    let o = r.run_out(&["-m", "b"], "true");
    assert!(
        !o.ok(),
        "files in outputs/ changed the recipe (should be auto-ignored):\n{o}"
    );
}

#[test]
fn custom_output_dir_from_config_toml() {
    let r = Repo::init();
    r.set_config("output_dirs", "[\"results/\"]");
    r.run(&["-m", "a"], "true");
    r.write("results/x.txt", "out\n");
    let o = r.run_out(&["-m", "b"], "true");
    assert!(
        !o.ok(),
        "results/ listed in output_dirs still changed the recipe:\n{o}"
    );
}

#[test]
fn gitignore_and_pollardignore_respected() {
    let r = Repo::init();
    r.write(".gitignore", ".pollard\n*.log\n");
    r.write(".pollardignore", "scratch/\n");
    r.run(&["-m", "a"], "true");
    r.write("train.log", "noise\n");
    r.write("scratch/tmp.py", "noise\n");
    let o = r.run_out(&["-m", "b"], "true");
    assert!(!o.ok(), "ignored files changed the recipe:\n{o}");
}

#[test]
fn file_over_10mb_is_excluded_from_code_with_warning() {
    let r = Repo::init();
    r.run(&["-m", "a"], "true");
    r.write("big.bin", prng_bytes(11 * MB, 1));
    let o = r.run_out(&["-m", "b", "--force"], "true");
    assert!(o.ok(), "{o}");
    assert!(
        o.stderr.contains("big.bin") || o.stdout.contains("big.bin"),
        "no warning naming the >10 MB file:\n{o}"
    );
}

#[test]
fn warns_on_more_than_500_new_files() {
    let r = Repo::init();
    r.run(&["-m", "a"], "true");
    for i in 0..501 {
        r.write(&format!("gen/f{i}.txt"), format!("{i}"));
    }
    let o = r.run_out(&["-m", "b"], "true");
    assert!(o.ok(), "{o}");
    assert!(
        o.stderr.contains("500")
            || o.stderr.contains("501")
            || o.stderr.to_lowercase().contains("new files"),
        "no >500-new-files warning:\n{o}"
    );
}

#[test]
fn diff_shows_config_and_code_deltas() {
    let (r, p, kids) = five_children();
    let d = r.ok(&["diff", &p, &kids[3]]).stdout;
    assert!(
        d.contains("depth") && d.contains("12") && d.contains("24"),
        "config delta missing:\n{d}"
    );
    assert!(d.contains("attn.py"), "code delta missing:\n{d}");
    assert!(!d.contains("act"), "unchanged key shown:\n{d}");
}

#[test]
fn diff_between_siblings_uses_parent_relative_view() {
    let (r, _p, kids) = five_children();
    let d = r.ok(&["diff", &kids[0], &kids[1]]).stdout;
    assert!(
        d.contains("depth") && d.contains("act"),
        "sibling diff should show both parent-relative deltas:\n{d}"
    );
}

#[test]
fn config_delta_uses_dotted_paths() {
    let r = Repo::init();
    r.write(
        "config.yaml",
        "model:\n  depth: 12\n  width: 64\nopt:\n  lr: 3\n",
    );
    let p = r.run(&["-m", "p"], "true");
    r.write(
        "config.yaml",
        "model:\n  depth: 24\n  width: 64\nopt:\n  lr: 3\n",
    );
    let c = r.run(&["-m", "c"], "true");
    let d = r.ok(&["diff", &p, &c]).stdout;
    assert!(
        d.contains("model.depth"),
        "dotted path `model.depth` missing:\n{d}"
    );
    assert!(r.ok(&["siblings", &p]).stdout.contains("model.depth"));
}

// ---------- fork leaves off-tree files alone ----------

#[test]
fn fork_restores_code_and_config_leaves_offtree_and_prints_id() {
    let (r, p, _kids) = five_children();
    r.write("NOTES.md", "edited after the fact\n");
    let o = r.ok(&["fork", &p, "--no-sync"]);
    assert_eq!(o.last_line(), p, "fork must print the node id last:\n{o}");
    assert_eq!(r.read("config.yaml"), "lr: 3\ndepth: 12\nact: relu\n");
    assert!(r.root.join("util.py").exists(), "util.py not restored");
    assert!(
        !r.root.join("attn.py").exists(),
        "attn.py (not in parent) left behind"
    );
    assert_eq!(
        r.read("NOTES.md"),
        "edited after the fact\n",
        "fork touched an off-tree file"
    );
    assert_eq!(r.current(), p);
}

/// v3 §3: README.md is off-tree by default (via `*.md`); `!README.md` opts it back in.
#[test]
fn readme_is_offtree_by_default_and_negation_opts_in() {
    let r = Repo::init();
    r.write("README.md", "v1\n");
    r.run(&["-m", "a"], "true");
    r.write("README.md", "v2\n");
    assert!(
        !r.run_out(&["-m", "b"], "true").ok(),
        "README.md edit bypassed the duplicate check"
    );
    r.set_config("offtree", "[\"*.md\", \"notes/\", \"!README.md\"]");
    r.write("README.md", "v3\n");
    let o = r.run_out(&["-m", "c"], "true");
    assert!(
        o.ok(),
        "with `!README.md`, README.md should be code and change the recipe:\n{o}"
    );
}
