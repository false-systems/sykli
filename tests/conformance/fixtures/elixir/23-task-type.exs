use Sykli

pipeline do
  task "build" do
    run "echo build"
    task_type :build
  end

  task "test" do
    run "echo test"
    task_type :test
    after_ ["build"]
  end

  task "lint" do
    run "echo lint"
    task_type :lint
    after_ ["test"]
  end

  task "format" do
    run "echo format"
    task_type :format
    after_ ["lint"]
  end

  task "scan" do
    run "echo scan"
    task_type :scan
    after_ ["format"]
  end

  task "package" do
    run "echo package"
    task_type :package
    after_ ["scan"]
  end

  task "publish" do
    run "echo publish"
    task_type :publish
    after_ ["package"]
  end

  task "deploy" do
    run "echo deploy"
    task_type :deploy
    after_ ["publish"]
  end

  task "migrate" do
    run "echo migrate"
    task_type :migrate
    after_ ["deploy"]
  end

  task "generate" do
    run "echo generate"
    task_type :generate
    after_ ["migrate"]
  end

  task "verify" do
    run "echo verify"
    task_type :verify
    after_ ["generate"]
  end

  task "cleanup" do
    run "echo cleanup"
    task_type :cleanup
    after_ ["verify"]
  end
end
|> Sykli.Emitter.validate!()
|> Sykli.Emitter.to_json()
|> IO.puts()
