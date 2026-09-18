"""Optional W&B / Neptune forwarders (extras `pollard-vcs[wandb]`, `pollard-vcs[neptune]`).
pollard owns the tree; these services only get a copy of the metrics."""


def start(service, run, **kwargs):
    """Start a foreign run named after the node. Returns (log_fn(values, step), foreign_id)."""
    if service == "wandb":
        import wandb

        kwargs.setdefault("name", run.id)
        w = wandb.init(**kwargs)
        return (lambda values, step: w.log(dict(values), step=step)), w.id
    if service == "neptune":
        import neptune

        kwargs.setdefault("name", run.id)
        n = neptune.init_run(**kwargs)

        def log(values, step):
            for k, v in values.items():
                n[k].append(v, step=step)

        return log, n["sys/id"].fetch()
    raise ValueError(f"unknown forwarder {service!r} (use 'wandb' or 'neptune')")
