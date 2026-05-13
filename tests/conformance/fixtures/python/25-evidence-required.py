from sykli import Pipeline, file_evidence_non_empty

p = Pipeline()
p.task("test").run("go test ./...").task_type("test").evidence_required([
    file_evidence_non_empty("coverage", "coverage.out"),
])
p.emit()
