use sykli::{Pipeline, TaskType};

fn main() {
    let mut p = Pipeline::new();

    p.task("build")
        .run("echo build")
        .task_type(TaskType::Build);
    p.task("test")
        .run("echo test")
        .task_type(TaskType::Test)
        .after(&["build"]);
    p.task("lint")
        .run("echo lint")
        .task_type(TaskType::Lint)
        .after(&["test"]);
    p.task("format")
        .run("echo format")
        .task_type(TaskType::Format)
        .after(&["lint"]);
    p.task("scan")
        .run("echo scan")
        .task_type(TaskType::Scan)
        .after(&["format"]);
    p.task("package")
        .run("echo package")
        .task_type(TaskType::Package)
        .after(&["scan"]);
    p.task("publish")
        .run("echo publish")
        .task_type(TaskType::Publish)
        .after(&["package"]);
    p.task("deploy")
        .run("echo deploy")
        .task_type(TaskType::Deploy)
        .after(&["publish"]);
    p.task("migrate")
        .run("echo migrate")
        .task_type(TaskType::Migrate)
        .after(&["deploy"]);
    p.task("generate")
        .run("echo generate")
        .task_type(TaskType::Generate)
        .after(&["migrate"]);
    p.task("verify")
        .run("echo verify")
        .task_type(TaskType::Verify)
        .after(&["generate"]);
    p.task("cleanup")
        .run("echo cleanup")
        .task_type(TaskType::Cleanup)
        .after(&["verify"]);

    p.emit();
}
