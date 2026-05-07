from sykli import Pipeline, exit_code, file_exists, file_non_empty

p = Pipeline()
p.task("test").run("go test ./...").task_type("test").success_criteria([
    exit_code(0),
    file_exists("coverage.out"),
])
p.task("package").run("go build -o dist/app ./...").task_type("package").success_criteria([
    file_non_empty("dist/app"),
]).after("test")
p.emit()
