defmodule Sykli.GitHub.SourceTest do
  use ExUnit.Case, async: false

  alias Sykli.GitHub.Source

  @fixture Path.expand("../../../priv/test_fixtures/github_source/simple", __DIR__)

  test "fake source copies a fixture repo and cleans it up" do
    context = %{
      repo: "false-systems/sykli",
      head_sha: "abc123",
      delivery_id: "source-test",
      run_id: "github:source-test"
    }

    assert {:ok, path} =
             Source.acquire(context, "installation-token",
               impl: Sykli.GitHub.Source.Fake,
               source_fixture: @fixture
             )

    assert File.exists?(Path.join(path, "sykli.exs"))

    assert :ok = Source.cleanup(path, impl: Sykli.GitHub.Source.Fake)
    refute File.exists?(path)
  end

  test "real cleanup refuses paths outside the sykli temp root" do
    assert :ok = Sykli.GitHub.Source.Real.cleanup("/tmp/not-sykli-source")
  end
end
