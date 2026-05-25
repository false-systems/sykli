# Runtime trust model

Sykli executes the commands a pipeline declares. **Where** those commands run
(`Sykli.Target`) and **how** they run (`Sykli.Runtime`) determine what isolation,
if any, applies. This document states the trust boundary so operators know what
Sykli does and does not protect against.

## The Shell runtime is not a security sandbox

Under the **Shell runtime** (the default local runtime), a task command runs as a
child process of the Sykli engine, with the **same operating-system privileges as
the user who invoked Sykli**. It can read, write, and execute anything that user
can — including files outside the workspace such as `/etc/passwd`, environment
secrets, and the network.

This is by design: the Shell runtime is for **trusted repository code** — the same
trust you already extend to `make`, `npm test`, or any build script in the repo.
It is *not* a boundary for running untrusted or third-party pipelines.

> **If you need to run untrusted code, use a container runtime** (Docker, Podman,
> or the Kubernetes target). Containers provide the process/filesystem isolation
> the Shell runtime does not. Select one via the runtime priority chain (see
> `docs/runtimes.md`).

## What Sykli *does* guarantee: path containment on its own file operations

Independent of the runtime's command isolation, **Sykli's own file handling is
contained to the task workspace.** Operations the *engine* performs — not the
user's command — reject paths that escape the workdir or are absolute, and refuse
to follow symlinks out of it:

- artifact/output copy (`Sykli.Target.Local.copy_artifact/4`),
- container/K8s mount sources,
- `success_criteria` file paths (`file_exists` / `file_non_empty`),
- `evidence_required` local file refs,

all resolve through `Path.expand` + a `path_within?(resolved, workdir <> "/")`
check (the trailing slash prevents prefix tricks) and reject symlinks. A relative
path that traverses out (`../../etc/passwd`) or an absolute path is refused with a
structured error (`path escapes task workdir` / `path must be relative to task
workdir`), not silently followed.

This is the containment guarantee the 0.2.0 path-traversal fix established, and it
is what the black-box `GH-004` case asserts — Sykli does not let a *declared
contract field* read or write outside the workspace. It is distinct from, and does
not imply, sandboxing of the *user's command* under the Shell runtime.

## Summary

| Concern | Shell runtime | Container/K8s runtime |
|---|---|---|
| User command can touch host files / network | **Yes** (trusted code only) | Isolated by the container |
| Sykli's *own* file ops escape the workspace | No — contained + symlink-rejecting | No — contained + symlink-rejecting |

Run untrusted pipelines in a container runtime. Treat the Shell runtime the way
you treat any build script you already run on your machine.
