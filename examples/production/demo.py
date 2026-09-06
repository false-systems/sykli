#!/usr/bin/env python3
"""Real native build; separate processes for each worker. No third-party packages."""
import argparse
import json
import pathlib
import platform
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve()
    root = pathlib.Path(tempfile.mkdtemp(prefix="sykli-production-demo-"))
    source = pathlib.Path(__file__).with_name("main.rs").read_text()
    (root / "main.rs").write_text(source)
    transcript = {"host": platform.platform(), "workspace": str(root), "commands": []}

    def run(arguments, expected=0):
        result = subprocess.run([str(binary), *arguments], cwd=root, capture_output=True, text=True)
        output = json.loads(result.stdout)
        transcript["commands"].append({"argv": ["sykli", *arguments], "exit": result.returncode,
                                       "stdout": output, "stderr": result.stderr})
        assert result.returncode == expected, (arguments, output, result.stderr)
        return output

    run(["init", "--production"])
    contract_path = root / "sykli.production.json"
    contract = json.loads(contract_path.read_text())
    smoke = contract["targets"]["app"]["operations"]["smoke_test"]
    smoke["run"] = 'test "$("$SYKLI_INPUT_executable")" = 42'
    smoke["assertion"] = "executable prints 42"
    contract_path.write_text(json.dumps(contract, indent=2))
    run(["targets", "--json"])
    planned = run(["plan", "sykli.production.json", "--target", "app", "--json"])
    first = run(["produce", "app", "--stop-after", "build", "--json"], 1)
    production = first["production"]
    assert production == planned["production"]
    assert first["through_sequence"] == 2
    # The previous client exited. This client has only the identifier and store.
    run(["status", production, "--json"])
    completed = run(["resume", production, "--json"])
    run(["verify-production", production, "--json"])
    assert first["work"]["build"] == completed["work"]["build"]
    location = completed["delivery"]["app"]["availability"]["locations"][0]
    artifact = subprocess.run([location], capture_output=True, text=True, check=True)
    assert artifact.stdout == "42\n"
    transcript["artifact_execution"] = {"path": location, "exit": artifact.returncode, "stdout": artifact.stdout}

    # The new source compiles but its declared source and artifact checks fail.
    (root / "main.rs").write_text('fn main() { println!("43"); }\n#[test] fn unit() { assert_eq!(43, 42); }\n')
    second = run(["produce", "app", "--json"], 1)
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
