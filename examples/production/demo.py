#!/usr/bin/env python3
"""Real native build; separate processes for each worker. No third-party packages."""
import argparse
import json
import pathlib
import platform
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--language", choices=["rust", "go"], default="rust")
    args = parser.parse_args()
    binary = args.binary.resolve()
    root = pathlib.Path(tempfile.mkdtemp(prefix="sykli-production-demo-"))
    if args.language == "go":
        fixture = pathlib.Path(__file__).resolve().parent.parent / "go"
        for name in ("go.mod", "main.go", "main_test.go"):
            shutil.copyfile(fixture / name, root / name)
    else:
        source = pathlib.Path(__file__).with_name("main.rs").read_text()
        (root / "main.rs").write_text(source)
    transcript = {"host": platform.platform(), "language": args.language,
                  "workspace": str(root), "commands": []}

    def run(arguments, expected=0):
        result = subprocess.run([str(binary), *arguments], cwd=root, capture_output=True, text=True)
        output = json.loads(result.stdout)
        transcript["commands"].append({"argv": ["sykli", *arguments], "exit": result.returncode,
                                       "stdout": output, "stderr": result.stderr})
        assert result.returncode == expected, (arguments, output, result.stderr)
        return output

    run(["init", "--production", "--smoke", 'test "$("$SYKLI_INPUT_executable")" = 42'])
    contract_path = root / "sykli.production.json"
    contract = json.loads(contract_path.read_text())
    smoke = contract["targets"]["app"]["operations"]["smoke_test"]
    smoke["run"] = 'test "$("$SYKLI_INPUT_executable")" = 42'
    smoke["assertion"] = "executable prints 42"
    contract_path.write_text(json.dumps(contract, indent=2))
    run(["targets", "--json"])
    planned = run(["plan", "sykli.production.json", "--target", "app", "--json"])
    prepared = run(["produce", "app", "--prepare", "--summary", "--json"])
    assert prepared["through_sequence"] == 0
    assert prepared["ready"] == ["build", "unit_tests"]
    first = run(["resume", prepared["production"], "--operation", "build", "--summary", "--json"], 1)
    production = first["production"]
    assert production == planned["production"]
    assert first["through_sequence"] == 2
    # The previous client exited. This client has only the identifier and store.
    run(["status", production, "--summary", "--json"])
    completed = run(["resume", production, "--jobs", "2", "--summary", "--json"])
    run(["verify-production", production, "--json"])
    assert first["work"]["build"] == completed["work"]["build"]
    for check, subject in (("unit_tests", first["inputs"]["source"]),
                           ("smoke_test", completed["delivery"]["app"]["artifact"])):
        diagnostics = run(["diagnostics", production,
                           completed["assessment"]["satisfied_checks"][check], "--json"])
        result = diagnostics["records"][-1]["record"]["fact"]["result"]
        assert result["subject"] == subject and result["outcome"] == "passed"
    location = completed["delivery"]["app"]["availability"]["locations"][0]
    artifact = subprocess.run([location], capture_output=True, text=True, check=True)
    assert artifact.stdout == "42\n"
    transcript["artifact_execution"] = {"path": location, "exit": artifact.returncode, "stdout": artifact.stdout}

    # The new source compiles but its declared source and artifact checks fail.
    if args.language == "go":
        source_path = root / "main.go"
        source_path.write_text(source_path.read_text().replace("return 42", "return 43"))
    else:
        (root / "main.rs").write_text('fn main() { println!("43"); }\n#[test] fn unit() { assert_eq!(43, 42); }\n')
    second = run(["produce", "app", "--jobs", "2", "--summary", "--json"], 1)
    assert second["production"] != production
    assert second["inputs"] != first["inputs"]
    assert second["assessment"]["kind"] == "incomplete"
    assert second["work"]["smoke_test"]["state"]["kind"] == "failed"
    assert second["work"]["unit_tests"]["state"]["kind"] == "failed"
    old = run(["status", production, "--json"])
    assert old["assessment"]["kind"] == "complete"
    transcript["summary"] = {
        "production_a": production, "source_a": first["inputs"]["source"],
        "artifact_a": completed["delivery"]["app"]["artifact"],
        "checks_a": completed["assessment"]["satisfied_checks"],
        "production_b": second["production"], "source_b": second["inputs"]["source"],
        "b_assessment": second["assessment"], "a_still_complete": True,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(transcript, indent=2) + "\n")
    print(json.dumps(transcript["summary"], indent=2))
    print("Saved commands and outputs:", args.output)
    print("Artifacts and records remain in:", root)


if __name__ == "__main__":
    main()
