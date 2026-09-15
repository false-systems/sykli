#!/usr/bin/env python3
"""Repository-local CI experiment. It never decides the required gate's result."""

import argparse
import hashlib
import html
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args], timeout=30)


def capture(binary, cwd, args, output, name, timeout=60):
    """Keep task output in files and kill the whole local attempt on timeout."""
    started = time.monotonic()
    suffix = ".txt" if name == "full-verification" else ".json"
    with (output / (name + suffix)).open("wb") as stdout, (
        output / (name + ".stderr")
    ).open("wb") as stderr:
        with subprocess.Popen(
            [str(binary), *args], cwd=cwd, stdout=stdout, stderr=stderr,
            start_new_session=True,
        ) as process:
            try:
                code = process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                raise RuntimeError(name + " timed out") from None
    return code, round((time.monotonic() - started) * 1000)


def read_json(output, name):
    return json.loads((output / (name + ".json")).read_bytes())


def compare(plan, receipt, affected, baseline_receipts):
    records = {record["name"]: record for record in receipt["tasks"]}
    rows = []
    for item in plan["explanations"]:
        name, cache = item["task"], item["cache"]
        record = records[name]
        comparison = "not_proposed"
        if cache["status"] == "available":
            # The planner checks this receipt; compare its runtime with the
            # independent full execution too, not merely with this inspection.
            baseline = json.loads((baseline_receipts / cache["receipt"]).read_bytes())
            previous = next(task for task in baseline["tasks"] if task["name"] == name)
            if (
                record["runtime_fingerprint"] != previous["runtime_fingerprint"]
                or record.get("inherited_digests", {}) != previous.get("inherited_digests", {})
            ):
                comparison = "context_mismatch"
            elif record["source"] != "task" or record["outcome"] == "blocked":
                comparison = "not_independently_executed"
            elif record["outcome"] == "passed" and record["importable"]:
                comparison = (
                    "agrees" if record["output_digests"] == previous["output_digests"]
                    else "disagrees"
                )
            elif record["outcome"] in ("failed", "errored"):
                comparison = "disagrees"
            else:
                comparison = "unproven"
        rows.append({
            "task": name,
            "affected": name in affected,
            "cache": cache,
            "full_outcome": record["outcome"],
            "full_source": record["source"],
            "full_duration_ms": record["duration_ms"],
            "comparison": comparison,
        })
    return rows


def experiment(args, output):
    repo = Path(git(Path.cwd(), "rev-parse", "--show-toplevel").decode().strip())
    base = git(repo, "rev-parse", "--verify", "--end-of-options", args.base + "^{commit}").decode().strip()
    candidate = git(repo, "rev-parse", "HEAD").decode().strip()
    binary = Path(args.binary).resolve(strict=True)
    receipt_path = Path(args.receipt).resolve(strict=True)
    full_bytes = receipt_path.read_bytes()
    receipt = json.loads(full_bytes)
    (output / "full-receipt.json").write_bytes(full_bytes)
    changes = git(repo, "diff", "--name-status", "-z", "--no-renames", base, candidate, "--")
    parts = changes.split(b"\0")[:-1]
    changed = [
        {"status": parts[i].decode("ascii"), "path": parts[i + 1].decode("utf-8")}
        for i in range(0, len(parts), 2)
    ]
    report = {
        "schema": "sykli-ci-shadow.v1",
        "status": "observed",
        "advisory": True,
        "base_commit": base,
        "candidate_commit": candidate,
        "pr_head": args.pr_head,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "full_receipt_sha256": hashlib.sha256(full_bytes).hexdigest(),
        "changes": changed,
    }
    write_json(output / "changes.json", report)

    with tempfile.TemporaryDirectory(prefix="sykli-shadow-") as temporary:
        temporary = Path(temporary)
        base_dir, candidate_dir = temporary / "base", temporary / "candidate"
        worktrees = []
        try:
            for path, commit in [(base_dir, base), (candidate_dir, candidate)]:
                git(repo, "worktree", "add", "--detach", str(path), commit)
                worktrees.append(path)
            # Verify against the exact committed candidate, not the PR head
            # when Actions actually tested a synthetic merge commit.
            code, _ = capture(binary, candidate_dir, [
                "verify", str(receipt_path), "--contract", "sykli.json",
            ], output, "full-verification")
            if code not in (0, 1):
                raise RuntimeError("full receipt does not verify against the candidate")
            code, elapsed = capture(binary, base_dir, [
                "run", "sykli.json", "--json",
            ], output, "base-receipt", timeout=600)
            if code not in (0, 1):
                raise RuntimeError("baseline graph could not be evaluated")
            report["baseline_run_ms"] = elapsed
            report["baseline_outcome"] = read_json(output, "base-receipt")["outcome"]
            # Only baseline evidence reaches the inspection checkout. The
            # authoritative full run's cache is never copied or reused.
            for name in ("cache", "receipts"):
                source = base_dir / ".sykli" / name
                if source.exists():
                    shutil.copytree(source, candidate_dir / ".sykli" / name)
            started = time.monotonic()
            code, _ = capture(binary, candidate_dir, [
                "plan", "--explain", "--json",
            ], output, "candidate-plan")
            if code not in (0, 2):
                raise RuntimeError("candidate explanation failed")
            plan = read_json(output, "candidate-plan")
            if plan["contract_hash"] != receipt["contract_hash"]:
                raise RuntimeError("candidate plan and full receipt describe different contracts")
            affected = []
            if changed:
                flags = [value for change in changed for value in ("--changed", change["path"])]
                code, _ = capture(binary, candidate_dir, [
                    "plan", "--explain", "--json", *flags,
                ], output, "changed-plan")
                if code not in (0, 2):
                    raise RuntimeError("changed-path explanation failed")
                affected = read_json(output, "changed-plan")["tasks"]
            else:
                write_json(output / "changed-plan.json", {"tasks": [], "reason": "no_changed_paths"})
            report["inspection_ms"] = round((time.monotonic() - started) * 1000)
            contract = json.loads((candidate_dir / "sykli.json").read_bytes())
            inputs = {
                os.path.normpath(os.path.join(task.get("workdir") or ".", path))
                for task in contract["tasks"] for path in task.get("inputs", [])
            }
            report["unmapped_paths"] = [
                change["path"] for change in changed if change["path"] not in inputs
            ]
            report["tasks"] = compare(
                plan, receipt, set(affected), candidate_dir / ".sykli/receipts",
            )
            # Keep provenance for every proposed reuse, without exporting
            # executable cache artifacts or restoring any candidate outputs.
            for row in report["tasks"]:
                if row["cache"]["status"] == "available":
                    path = candidate_dir / ".sykli/receipts" / row["cache"]["receipt"]
                    destination = output / "baseline-receipts" / path.name
                    destination.parent.mkdir(exist_ok=True)
                    shutil.copyfile(path, destination)
            report["disagreements"] = [
                row["task"] for row in report["tasks"] if row["comparison"] == "disagrees"
            ]
            report["potential_reused_task_ms"] = sum(
                row["full_duration_ms"] for row in report["tasks"] if row["comparison"] == "agrees"
            )
            report["full_task_ms"] = sum(row["full_duration_ms"] for row in report["tasks"])
        finally:
            for path in reversed(worktrees):
                git(repo, "worktree", "remove", "--force", str(path))
    return report


def summary(report):
    lines = ["### Sykli CI shadow experiment", "", "Advisory; the full gate remains authoritative.", ""]
    if report["status"] != "observed":
        lines += ["Observation unavailable: " + html.escape(report["error"]), ""]
        return "\n".join(lines)
    lines += [
        "Compared base <code>" + report["base_commit"] + "</code> with tested candidate <code>"
        + report["candidate_commit"] + "</code>.", "",
        "| task | affected | cache evidence | full result | comparison |",
        "|---|---|---|---|---|",
    ]
    for row in report["tasks"]:
        name = html.escape(row["task"]).replace("|", "&#124;").replace("\n", "&#10;")
        lines.append("| <code>" + name + "</code> | " + str(row["affected"]).lower()
                     + " | " + row["cache"]["status"] + " | " + row["full_outcome"]
                     + " | " + row["comparison"] + " |")
    lines += [
        "",
        "Potential reuse: **" + str(report["potential_reused_task_ms"])
        + " ms of task duration**, from " + str(report["full_task_ms"])
        + " ms observed. Task durations overlap; this is not job wall time saved.",
        "Baseline cost: " + str(report["baseline_run_ms"]) + " ms; inspection: "
        + str(report["inspection_ms"]) + " ms.",
        "Reuse disagreements: **" + str(len(report["disagreements"])) + "**.",
        "Changed paths without an exact declared input: **"
        + str(len(report["unmapped_paths"])) + "** (see changes.json and report.json).",
        "",
        "A matching observation is not proof that the input declarations are complete.",
        "",
    ]
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--receipt", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--pr-head", default=None)
    args = parser.parse_args()
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    started = time.monotonic()
    try:
        report = experiment(args, output)
    except (OSError, ValueError, KeyError, StopIteration, RuntimeError, subprocess.SubprocessError) as error:
        report = {"schema": "sykli-ci-shadow.v1", "status": "unavailable",
                  "advisory": True, "error": str(error)}
    report["experiment_ms"] = round((time.monotonic() - started) * 1000)
    write_json(output / "report.json", report)
    rendered = summary(report)
    (output / "summary.md").write_text(rendered)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as stream:
            stream.write(rendered)
    print(json.dumps({"status": report["status"], "report": str(output / "report.json")}))
    return 0 if report["status"] == "observed" else 2


if __name__ == "__main__":
    raise SystemExit(main())
