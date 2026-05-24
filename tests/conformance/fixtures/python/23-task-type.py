from sykli import Pipeline

p = Pipeline()

p.task("build").run("echo build").task_type("build")
p.task("test").run("echo test").task_type("test").after("build")
p.task("lint").run("echo lint").task_type("lint").after("test")
p.task("format").run("echo format").task_type("format").after("lint")
p.task("scan").run("echo scan").task_type("scan").after("format")
p.task("package").run("echo package").task_type("package").after("scan")
p.task("publish").run("echo publish").task_type("publish").after("package")
p.task("deploy").run("echo deploy").task_type("deploy").after("publish")
p.task("migrate").run("echo migrate").task_type("migrate").after("deploy")
p.task("generate").run("echo generate").task_type("generate").after("migrate")
p.task("verify").run("echo verify").task_type("verify").after("generate")
p.task("cleanup").run("echo cleanup").task_type("cleanup").after("verify")

p.emit()
