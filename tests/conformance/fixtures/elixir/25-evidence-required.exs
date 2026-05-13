use Sykli

pipeline do
  task "test" do
    run("go test ./...")
    task_type(:test)

    evidence_required([
      file_evidence_non_empty("coverage", "coverage.out")
    ])
  end
end
