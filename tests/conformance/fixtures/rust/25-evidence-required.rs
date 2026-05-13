use sykli::{EvidenceRequirement, Pipeline, TaskType};

fn main() {
    let mut p = Pipeline::new();
    p.task("test")
        .run("go test ./...")
        .task_type(TaskType::Test)
        .evidence_required(&[EvidenceRequirement::file_non_empty("coverage", "coverage.out")]);
    p.emit();
}
