#!/bin/sh
# Build the alpha.1 fixture (BACKLOG F0) with the PUBLISHED pollard 0.1.0-alpha.1 binary.
#
# Regenerate (only needed if the fixture's contents change):
#   cargo install pollard-cli --version 0.1.0-alpha.1 --locked --root /some/prefix
#   POLLARD_ALPHA1_BIN=/some/prefix/bin/pollard crates/pollard-cli/tests/fixtures/alpha1/make.sh
# Needs: sh, setsid, tar, gzip, sqlite3. Writes repo.tar.gz, remote.tar.gz and golden/ next to
# this script. Ids and timestamps change on every regeneration; tests read them from golden/.
#
# Layout inside the tarballs: repo.tar.gz unpacks to `repo/` (a working copy whose config.toml
# has `remote = "../remote"`), remote.tar.gz to `remote/`. Unpack both into one directory.
set -eu

PO=${POLLARD_ALPHA1_BIN:-pollard}
"$PO" --version | grep -qx 'pollard 0.1.0-alpha.1' || {
    echo "make.sh: $PO is not pollard 0.1.0-alpha.1 (set POLLARD_ALPHA1_BIN)" >&2
    exit 1
}
OUT=$(cd "$(dirname "$0")" && pwd)
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
export NO_COLOR=1 EDITOR=true VISUAL=true GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
# Machine-independent env hash, so a test can rerun a fixture recipe and hit the duplicate
# check: a checked-in uv.lock (no `uv pip freeze`) and a fixed CUDA string (tests set it too).
export POLLARD_CUDA=none

# Last stdout line of a mutating command is the node id.
id() { "$PO" "$@" | tail -n 1; }
cfg() { printf 'lr: %s\n' "$1" > config.yaml; }

mkdir -p "$W/repo" "$W/remote" "$W/clone2"
cd "$W/repo"
cat > train.sh <<'EOF'
emit() { s=$1; shift; line="{\"step\": $s"; for kv in "$@"; do line="$line, \"${kv%%=*}\": ${kv#*=}"; done; echo "$line}" >> "$POLLARD_METRICS"; }
emit 1 loss=2.0 acc=0.1
emit 2 loss=1.5 acc=0.2
case "${MODE:-ok}" in
  ckpt) printf 'fake weights %s\n' "$POLLARD_NODE_ID" > "$POLLARD_CKPT_DIR/step2.pt" ;;
  fail) exit 3 ;;
  kill) kill -INT 0; sleep 5 ;;
esac
EOF
cat > pyproject.toml <<'EOF'
[project]
name = "fixture"
version = "0.0.0"
requires-python = ">=3.8"
dependencies = []
EOF
cat > uv.lock <<'EOF'
version = 1
requires-python = ">=3.8"

[[package]]
name = "fixture"
version = "0.0.0"
source = { virtual = "." }
EOF
cfg 0
"$PO" init >/dev/null
printf 'remote = "../remote"\n' >> .pollard/config.toml

ROOT=$(id run -m root -- sh train.sh)
cfg 1
BASE=$(MODE=ckpt id run -m base -- sh train.sh)
"$PO" pin "$ROOT" root-pin >/dev/null

"$PO" fork "$ROOT" --no-sync >/dev/null
cfg 2
MID=$(id run -m mid -- sh train.sh)
cfg 3
BEST=$(MODE=ckpt id run -m best -- sh train.sh)
"$PO" pin "$BEST" paper >/dev/null
"$PO" pin "$BEST" best-v1 >/dev/null

"$PO" fork "$MID" --no-sync >/dev/null
cfg 4
CRASHED=$(MODE=fail "$PO" run -m crashed -- sh train.sh | tail -n 1) || true
CRASHED=$("$PO" show @ | grep -o '[a-z]*-[a-z]*-[0-9]*' | head -n 1)

"$PO" fork "$MID" --no-sync >/dev/null
cfg 5
# SIGINT to the whole process group, like Ctrl-C (setsid gives pollard its own group).
MODE=kill setsid -w "$PO" run -m stopped -- sh train.sh >/dev/null 2>&1 || true
STOPPED=$("$PO" show @ | grep -o '[a-z]*-[a-z]*-[0-9]*' | head -n 1)
# Their real outcomes, before the prune hides them (migration must recover these).
"$PO" show "$CRASHED" | grep -qx 'status    failed'
"$PO" show "$STOPPED" | grep -qx 'status    killed'

# The #19 repro: prune the subtree while mid is @, then run from the pruned @.
"$PO" fork "$MID" --no-sync >/dev/null
"$PO" prune "$MID" >/dev/null
cfg 6
LATE=$(id run -m late -- sh train.sh)

"$PO" fork "$ROOT" --no-sync >/dev/null
cfg 7
KEPT=$(MODE=ckpt id run -m kept -- sh train.sh)
"$PO" prune "$KEPT" --keep-weights >/dev/null
"$PO" push >/dev/null

# ghost: made and pruned in a second clone, pushed, then pulled here (no prune op here).
cd "$W/clone2"
"$PO" init >/dev/null
printf 'remote = "../remote"\n' >> .pollard/config.toml
"$PO" pull >/dev/null
"$PO" fork "$ROOT" --no-sync >/dev/null
cfg 8
GHOST=$(id run -m ghost -- sh train.sh)
"$PO" prune "$GHOST" >/dev/null
"$PO" push >/dev/null
cd "$W/repo"
"$PO" pull >/dev/null

# redo: pruned, undone, pruned again (recovery must use the latest prune).
"$PO" fork "$ROOT" --no-sync >/dev/null
cfg 9
REDO=$(id run -m redo -- sh train.sh)
"$PO" prune "$REDO" >/dev/null
"$PO" undo >/dev/null
"$PO" prune "$REDO" >/dev/null
# Leave @ on late (the node made under the pruned @).
"$PO" fork "$LATE" --no-sync >/dev/null
"$PO" push >/dev/null

# Sanity: every role got a distinct node id.
for v in "$ROOT" "$BASE" "$MID" "$BEST" "$CRASHED" "$STOPPED" "$LATE" "$KEPT" "$GHOST" "$REDO"; do
    case "$v" in *-*-*) ;; *) echo "make.sh: bad id '$v'" >&2; exit 1 ;; esac
done
[ "$(printf '%s\n' "$ROOT" "$BASE" "$MID" "$BEST" "$CRASHED" "$STOPPED" "$LATE" "$KEPT" "$GHOST" "$REDO" | sort -u | wc -l)" -eq 10 ]
"$PO" show "$CRASHED" | grep -q pruned
"$PO" show "$STOPPED" | grep -q pruned

# Golden outputs (alpha.1's view of the finished fixture).
G="$W/golden"
mkdir -p "$G"
cat > "$G/ids.json" <<EOF
{
  "root": "$ROOT",
  "base": "$BASE",
  "mid": "$MID",
  "best": "$BEST",
  "crashed": "$CRASHED",
  "stopped": "$STOPPED",
  "late": "$LATE",
  "kept": "$KEPT",
  "ghost": "$GHOST",
  "redo": "$REDO"
}
EOF
"$PO" tree --all > "$G/tree_all.txt"
"$PO" tree > "$G/tree.txt"
for role in root base mid best crashed stopped late kept ghost redo; do
    eval "n=\$$(echo "$role" | tr a-z A-Z)"
    "$PO" show "$n" > "$G/show_$role.txt"
done
"$PO" siblings "$BASE" --json > "$G/siblings_base.json"
"$PO" siblings "$ROOT" --json > "$G/siblings_root.json"
"$PO" op log > "$G/op_log.txt"
sqlite3 .pollard/db.sqlite 'SELECT * FROM pins ORDER BY name' > "$G/pins.txt"
sqlite3 .pollard/db.sqlite 'SELECT id, status FROM nodes ORDER BY id' > "$G/status.txt"
for t in nodes pins ops; do
    printf '%s %s\n' "$t" "$(sqlite3 .pollard/db.sqlite "SELECT count(*) FROM $t")"
done > "$G/counts.txt"
cp .pollard/config.toml "$G/config.toml"

# No stray WAL: alpha.1 checkpoints on close; make sure before packing.
sqlite3 .pollard/db.sqlite 'PRAGMA wal_checkpoint(TRUNCATE);' >/dev/null
rm -f .pollard/db.sqlite-wal .pollard/db.sqlite-shm

cd "$W"
rm -rf "$OUT/golden"
cp -r "$G" "$OUT/golden"
tar --sort=name --owner=0 --group=0 --numeric-owner -czf "$OUT/repo.tar.gz" repo
tar --sort=name --owner=0 --group=0 --numeric-owner -czf "$OUT/remote.tar.gz" remote
echo "fixture written to $OUT"
