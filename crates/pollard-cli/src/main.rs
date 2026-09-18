//! `pollard` / `po`: thin CLI over pollard-core. Every mutating command prints the id of
//! the node it created or changed on its last stdout line.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use pollard_core::{Repo, cmd, gitops, metrics, ops, run, siblings, sync, tree, weights};

#[derive(Parser)]
#[command(name = "pollard", version, about = "Tree-based experiment version control")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create .pollard/ and add it to .gitignore
    Init {
        /// Import git HEAD as the root node
        #[arg(long)]
        from_git: bool,
    },
    /// Snapshot the working copy into a new node and launch <cmd>
    Run {
        /// Note paragraph (repeatable); `-` reads stdin
        #[arg(short = 'm', long = "message")]
        message: Vec<String>,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        sweep: Option<String>,
        /// Run even if an identical recipe already ran
        #[arg(long)]
        force: bool,
        /// Refuse when `uv lock --check` fails
        #[arg(long)]
        strict: bool,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
    /// Restore a node's code and config and make it current
    Fork {
        node: String,
        #[arg(long)]
        step: Option<i64>,
        #[arg(long)]
        no_sync: bool,
    },
    /// Compare two nodes
    Diff {
        a: String,
        b: String,
        /// Show off-tree document changes instead
        #[arg(long)]
        docs: bool,
    },
    /// Sibling table for a node's children (default: parent of @)
    #[command(alias = "sib")]
    Siblings {
        node: Option<String>,
        #[arg(long)]
        metric: Option<String>,
        #[arg(long)]
        expand_sweeps: bool,
        /// Include pruned children
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Render the tree
    Tree {
        #[arg(long)]
        metric: Option<String>,
        /// Show pruned and failed subtrees
        #[arg(long)]
        all: bool,
    },
    /// Full record of a node
    Show { node: String },
    /// Mark a subtree pruned
    Prune {
        node: String,
        #[arg(long)]
        keep_weights: bool,
    },
    /// Name a node
    Pin { node: String, name: String },
    /// Remove a pin
    Unpin { name: String },
    /// Set or replace a node's note
    Note {
        node: String,
        text: Option<String>,
        /// Edit in $EDITOR
        #[arg(short = 'e', long)]
        edit: bool,
        /// Append a line to the existing note
        #[arg(long, conflicts_with_all = ["text", "edit"])]
        append: Option<String>,
    },
    /// Apply a node's code delta to the working copy
    Apply { node: String },
    /// Print a metric stream (inherited points resolved)
    Log {
        node: String,
        #[arg(long)]
        key: Option<String>,
    },
    /// Register a checkpoint now (from inside a running script)
    Ckpt { paths: Vec<PathBuf> },
    /// Register another output now (from inside a running script)
    Artifact { paths: Vec<PathBuf> },
    /// Restore repo state to before the last n ops
    Undo {
        #[arg(default_value_t = 1)]
        n: usize,
    },
    /// Op log
    Op {
        #[command(subcommand)]
        cmd: OpCmd,
    },
    /// Create a root node from a git commit
    Import { rev: String },
    /// Write git commits for a node or an ancestry path
    Export {
        node: Option<String>,
        /// `a..b`: one commit per node from a down to b
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Upload new nodes, objects and chunks to the remote
    Push,
    /// Fetch nodes, objects and chunks from the remote
    Pull,
    /// Delete unreferenced chunks
    Gc {
        #[arg(long)]
        auto: bool,
    },
}

#[derive(Subcommand)]
enum OpCmd {
    /// List recent ops
    Log {
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

fn open() -> Result<Repo> {
    let mut repo = Repo::discover(&std::env::current_dir()?)?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    repo.cmdline = format!("pollard {}", args.join(" "));
    Ok(repo)
}

/// Node that `ckpt`/`artifact` attach to: the running node, else `@`.
fn running_node() -> String {
    std::env::var("POLLARD_NODE_ID").unwrap_or_else(|_| "@".into())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli.cmd) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn dispatch(c: Cmd) -> Result<u8> {
    match c {
        Cmd::Init { from_git } => {
            let cwd = std::env::current_dir()?;
            let repo = Repo::init(&cwd)?;
            println!("initialized {}", repo.dot.display());
            if from_git {
                let mut repo = open()?;
                let id = gitops::import(&mut repo, "HEAD")?.node;
                println!("{}", root_summary(&repo, &id)?);
                println!("{id}");
            }
        }
        Cmd::Run { message, parent, sweep, force, strict, cmd } => {
            let mut repo = open()?;
            let mut notes = vec![];
            for m in message {
                if m == "-" {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    notes.push(s.trim_end().to_string());
                } else {
                    notes.push(m);
                }
            }
            let (op, l) = run::start(&mut repo, &run::RunOpts { notes, parent, sweep, force, strict, cmd })?;
            let parent = l.parent.as_deref().map(|p| format!("  ← {p}")).unwrap_or_default();
            let at = l.fork_step.map(|s| format!(" @{s}")).unwrap_or_default();
            println!("node  {}{parent}{at}   launching: {}", op.node, l.argv.join(" "));
            if let Some(n) = &l.note_auto {
                println!("note(auto): {n}");
            }
            let code = run::execute(&repo, &l)?;
            println!("{}", op.node);
            return Ok(code.clamp(0, 255) as u8);
        }
        Cmd::Fork { node, step, no_sync } => {
            let mut repo = open()?;
            let f = cmd::fork(&mut repo, &node, step, no_sync)?;
            for n in &f.notices {
                println!("{n}");
            }
            if let Some((p, _)) = &f.restored {
                println!("restored {p}");
            }
            if f.synced {
                println!("uv sync --frozen ✓");
            }
            if let Some(s) = f.fork_step {
                println!("fork_step={s}");
            }
            println!("{}", f.op.node);
        }
        Cmd::Diff { a, b, docs } => {
            let repo = open()?;
            print!("{}", cmd::diff(&repo, &a, &b, docs)?);
        }
        Cmd::Siblings { node, metric, expand_sweeps, all, json } => {
            let repo = open()?;
            let parent = match node {
                Some(n) => repo.resolve(&n)?,
                None => repo.resolve("@-")?,
            };
            let t = siblings::build(&repo, &parent, &siblings::Opts { metric, expand_sweeps, all, only: None })?;
            if json {
                println!("{}", siblings::to_json(&t));
            } else if t.columns.is_empty() {
                println!("{parent} has no children");
            } else {
                print!("{}", siblings::render(&t));
            }
        }
        Cmd::Tree { metric, all } => {
            let repo = open()?;
            print!("{}", tree::render(&repo, &tree::Opts { metric, all })?);
        }
        Cmd::Show { node } => {
            let repo = open()?;
            print!("{}", show(&repo, &node)?);
        }
        Cmd::Prune { node, keep_weights } => {
            let mut repo = open()?;
            println!("{}", weights::prune(&mut repo, &node, keep_weights)?.node);
        }
        Cmd::Pin { node, name } => {
            let mut repo = open()?;
            println!("{}", cmd::pin(&mut repo, &node, &name)?.node);
        }
        Cmd::Unpin { name } => {
            let mut repo = open()?;
            println!("{}", cmd::unpin(&mut repo, &name)?.node);
        }
        Cmd::Note { node, text, edit, append } => {
            let mut repo = open()?;
            let id = repo.resolve(&node)?;
            let text = match (text, edit, append) {
                (_, _, Some(a)) => match repo.node(&id)?.note.filter(|n| !n.is_empty()) {
                    Some(n) => format!("{n}\n{a}"),
                    None => a,
                },
                (Some(t), false, None) => t,
                (_, true, None) => edit_note(&repo, &id)?,
                (None, false, None) => bail!("note: give the text, -e or --append"),
            };
            println!("{}", cmd::note(&mut repo, &id, &text)?.node);
        }
        Cmd::Apply { node } => {
            let mut repo = open()?;
            let a = cmd::apply(&mut repo, &node)?;
            for p in &a.changed {
                println!("M {p}");
            }
            if !a.conflicts.is_empty() {
                eprintln!("warning: conflicts in {} (markers left in place)", a.conflicts.join(", "));
            }
            println!("{}", a.op.node);
        }
        Cmd::Log { node, key } => {
            let repo = open()?;
            let id = repo.resolve(&node)?;
            let keys = match key {
                Some(k) => vec![k],
                None => metrics::keys(&repo.db, &id)?,
            };
            let single = keys.len() == 1;
            for k in keys {
                for (s, v) in metrics::series(&repo.db, &id, &k)? {
                    if single { println!("{s}\t{v}") } else { println!("{k}\t{s}\t{v}") }
                }
            }
        }
        Cmd::Ckpt { paths } | Cmd::Artifact { paths } => {
            if paths.is_empty() {
                bail!("give at least one path");
            }
            let mut repo = open()?;
            println!("{}", weights::attach(&mut repo, &running_node(), &paths)?.node);
        }
        Cmd::Undo { n } => {
            let mut repo = open()?;
            println!("{}", ops::undo(&mut repo, n)?.node);
        }
        Cmd::Op { cmd: OpCmd::Log { limit } } => {
            let repo = open()?;
            for e in ops::log(&repo, limit)? {
                println!("{:>5}  {}  {}", e.op_id, e.ts, e.command);
            }
        }
        Cmd::Import { rev } => {
            let mut repo = open()?;
            println!("{}", gitops::import(&mut repo, &rev)?.node);
        }
        Cmd::Export { node, path, branch } => {
            let repo = open()?;
            let spec = path.or(node).unwrap_or_else(|| "@".into());
            let ex = gitops::export(&repo, &spec, branch.as_deref())?;
            for (n, c) in &ex.commits {
                println!("{n} {}", &c[..7.min(c.len())]);
            }
            println!("branch {}", ex.branch);
            if let Some((n, _)) = ex.commits.last() {
                println!("{n}");
            }
        }
        Cmd::Push => {
            let repo = open()?;
            let st = sync::push(&repo)?;
            println!("pushed {} nodes, {} bytes", st.nodes, st.blobs.bytes);
        }
        Cmd::Pull => {
            let mut repo = open()?;
            let (rec, st) = sync::pull(&mut repo)?;
            println!("pulled {} nodes, {} bytes", st.nodes, st.blobs.bytes);
            println!("{}", rec.node);
        }
        Cmd::Gc { auto: _ } => {
            let repo = open()?;
            let st = weights::gc(&repo)?;
            println!(
                "gc: removed {} chunks, {} chunk maps, freed {} bytes",
                st.chunks_removed, st.maps_removed, st.bytes_freed
            );
        }
    }
    Ok(0)
}

/// Journey A's `root <id> (git abc1234) code ✓ config ✓ data N files ✓ env uv.lock ✓ offtree: …` line.
fn root_summary(repo: &Repo, id: &str) -> Result<String> {
    let n = repo.node(id)?;
    let git = n.note.as_deref().and_then(|t| t.split("(git ").nth(1)).map(|t| t.trim_end_matches(')')).unwrap_or("?");
    let data = repo.objects.get_manifest(&n.data)?.entries.len();
    let env = if repo.root.join("uv.lock").is_file() { "uv.lock" } else { "freeze" };
    let docs: Vec<String> = match &n.docs {
        Some(d) => repo.objects.get_manifest(d)?.entries.into_iter().map(|e| e.path).collect(),
        None => vec![],
    };
    let off = if docs.is_empty() { String::new() } else { format!(" offtree: {}", docs.join(", ")) };
    Ok(format!("root  {id}  (git {git})  code ✓ config ✓ data {data} files ✓ env {env} ✓{off}"))
}

fn edit_note(repo: &Repo, id: &str) -> Result<String> {
    let path = repo.dot.join(format!("NOTE_EDITMSG-{id}"));
    std::fs::write(&path, repo.node(id)?.note.unwrap_or_default())?;
    let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| "vi".into());
    let st = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&path)
        .status()
        .with_context(|| format!("launching editor {editor}"))?;
    if !st.success() {
        bail!("editor {editor} exited with {st}");
    }
    let text = std::fs::read_to_string(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(text.trim_end().to_string())
}

fn show(repo: &Repo, node: &str) -> Result<String> {
    use std::fmt::Write;
    let n = repo.node(&repo.resolve(node)?)?;
    let mut o = String::new();
    let pins = repo.pins_of(&n.id)?;
    writeln!(o, "node      {}{}", n.id, if pins.is_empty() { String::new() } else { format!("  [{}]", pins.join(", ")) })?;
    writeln!(o, "parent    {}{}", n.parent.as_deref().unwrap_or("(root)"), n.fork_step.map(|s| format!(" @{s}")).unwrap_or_default())?;
    writeln!(o, "status    {}", n.status.as_str())?;
    if let Some(s) = &n.sweep {
        writeln!(o, "sweep     {s}")?;
    }
    writeln!(o, "created   {}", n.created_at)?;
    if let Some(f) = &n.finished_at {
        writeln!(o, "finished  {f}")?;
    }
    writeln!(o, "command   {}", n.command)?;
    writeln!(o, "code      {}", n.code)?;
    writeln!(o, "config    {}", n.config)?;
    writeln!(o, "data      {}", n.data)?;
    writeln!(o, "env       {}", n.env)?;
    writeln!(o, "recipe    {}", n.recipe_hash)?;
    if let Some(ok) = n.lock_ok {
        writeln!(o, "lock_ok   {ok}")?;
    }
    if let Some(note) = &n.note {
        writeln!(o, "note{}", if n.note_auto { " (auto)" } else { "" })?;
        for l in note.lines() {
            writeln!(o, "  {l}")?;
        }
    }
    let keys = metrics::keys(&repo.db, &n.id)?;
    let mut last_step = None;
    if !keys.is_empty() {
        writeln!(o, "metrics")?;
        for k in keys {
            let s = metrics::series(&repo.db, &n.id, &k)?;
            if let Some(&(st, v)) = s.last() {
                last_step = last_step.max(Some(st));
                writeln!(o, "  {k}  {}  @{}  ({} points)", siblings::fmt_num(v), st, s.len())?;
            }
        }
    }
    if let Some(s) = last_step {
        writeln!(o, "last step {s}")?;
    }
    if let Some(w) = &n.weights {
        let m = repo.objects.get_manifest(w)?;
        let ck = weights::checkpoints(repo, &n)?;
        if !ck.is_empty() {
            let steps: Vec<String> = ck.iter().map(|(e, s)| s.map_or(e.path.clone(), siblings::fmt_step)).collect();
            writeln!(o, "checkpoints: {}", steps.join(" "))?;
        }
        writeln!(o, "weights   {w}")?;
        for e in &m.entries {
            writeln!(o, "  {}  {} bytes", e.path, e.size)?;
        }
    }
    if let Some(d) = &n.docs {
        writeln!(o, "docs      {d}")?;
        for e in repo.objects.get_manifest(d)?.entries {
            writeln!(o, "  {}", e.path)?;
        }
    }
    writeln!(o, "reproduce: pollard fork {} && uv sync --frozen", n.id)?;
    Ok(o)
}
