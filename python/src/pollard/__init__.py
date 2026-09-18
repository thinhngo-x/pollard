"""pollard: pure-Python SDK over the run protocol (SPEC §4).

It only reads the POLLARD_* environment variables set by `pollard run` and writes the
protocol files; everything here is doable with plain file I/O.
"""

import dataclasses
import json
import os
import shutil
import subprocess

__all__ = ["Run", "current", "fork_step", "siblings", "forward"]

_run = None


def fork_step():
    """Step this run resumes from (`pollard fork --step`), or None."""
    s = os.environ.get("POLLARD_FORK_STEP")
    return int(s) if s else None


def _plain(cfg):
    """Config adapters: dict, OmegaConf/Hydra DictConfig, dataclass, argparse Namespace."""
    if hasattr(cfg, "_metadata") and type(cfg).__module__.startswith("omegaconf"):
        from omegaconf import OmegaConf

        return OmegaConf.to_container(cfg, resolve=True)
    if dataclasses.is_dataclass(cfg) and not isinstance(cfg, type):
        return dataclasses.asdict(cfg)
    if hasattr(cfg, "__dict__") and not isinstance(cfg, dict):
        return vars(cfg)
    return dict(cfg)


class Run:
    """The node this process is, as set up by `pollard run`."""

    def __init__(self, env=os.environ):
        if "POLLARD_NODE_ID" not in env:
            raise RuntimeError("not launched via `pollard run` (POLLARD_NODE_ID is unset)")
        self.id = env["POLLARD_NODE_ID"]
        self.metrics_path = env.get("POLLARD_METRICS")
        self.ckpt_dir = env.get("POLLARD_CKPT_DIR")
        self.config_path = env.get("POLLARD_CONFIG")
        self.fork_step = fork_step()
        self._step = self.fork_step or 0
        self._metrics = None
        self._forwarders = []

    def log(self, values, step=None):
        """Append one `{"step": int, key: number, ...}` line to POLLARD_METRICS."""
        if step is None:
            step = self._step + 1
        self._step = int(step)
        line = {"step": self._step}
        line.update({k: float(v) for k, v in values.items()})
        if self._metrics is None:
            self._metrics = open(self.metrics_path, "a")
        self._metrics.write(json.dumps(line) + "\n")
        self._metrics.flush()
        for f in self._forwarders:
            f(values, self._step)

    def save_checkpoint(self, path):
        """Put a checkpoint file into POLLARD_CKPT_DIR (copied if it lives elsewhere);
        it is registered in the node's weights at run end. Returns its path there."""
        src = os.path.abspath(path)
        ckpt = os.path.abspath(self.ckpt_dir)
        if os.path.commonpath([src, ckpt]) == ckpt:
            return src
        dest = os.path.join(ckpt, os.path.basename(src))
        os.makedirs(ckpt, exist_ok=True)
        shutil.copyfile(src, dest)
        return dest

    def set_config(self, cfg):
        """Record the resolved config (config capture mode `sdk`). Call before the first log."""
        tmp = self.config_path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(_plain(cfg), f, sort_keys=True, default=str)
        os.replace(tmp, self.config_path)

    def append_note(self, text):
        """Append a line to this node's note (via the CLI)."""
        _cli("note", self.id, "--append", text)


def current():
    """The current run; raises RuntimeError when not launched via `pollard run`."""
    global _run
    if _run is None:
        _run = Run()
    return _run


def _bin():
    for b in (os.environ.get("POLLARD_BIN"), shutil.which("pollard"), shutil.which("po")):
        if b:
            return b
    raise RuntimeError("pollard binary not found (install pollard-vcs or set POLLARD_BIN)")


def _cli(*args):
    return subprocess.run([_bin(), *args], check=True, capture_output=True, text=True).stdout


def siblings(node="@-", metric=None):
    """Sibling table of `node`'s children: a pandas DataFrame (rows = fields, columns =
    children) if pandas is installed, else the `{child: {row: cell}}` dict."""
    args = ["siblings", node, "--json"] + (["--metric", metric] if metric else [])
    table = json.loads(_cli(*args))
    try:
        import pandas
    except ImportError:
        return table
    return pandas.DataFrame(table)


def forward(service, **kwargs):
    """Mirror every `log` call to W&B ("wandb") or Neptune ("neptune") and record the
    foreign run id in this node's note. Extra kwargs go to wandb.init / neptune.init_run."""
    from . import _forward

    run = current()
    fn, foreign_id = _forward.start(service, run, **kwargs)
    run._forwarders.append(fn)
    try:
        run.append_note(f"{service}: {foreign_id}")
    except (OSError, RuntimeError, subprocess.CalledProcessError) as e:
        print(f"pollard: could not record the {service} run id in the note: {e}")
    return foreign_id
