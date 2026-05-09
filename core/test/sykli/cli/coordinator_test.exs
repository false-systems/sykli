defmodule Sykli.CLI.CoordinatorTest do
  use ExUnit.Case, async: true

  import ExUnit.CaptureIO

  alias Sykli.CLI.Coordinator

  test "help documents explicit bind behavior" do
    output = capture_io(fn -> assert Coordinator.run(["--help"]) == 0 end)

    assert output =~ "--bind ADDRESS"
    assert output =~ "default: 127.0.0.1"
    assert output =~ "use 0.0.0.0 only intentionally"
  end

  test "invalid bind returns structured JSON error" do
    output =
      capture_io(fn ->
        assert Coordinator.run(["start", "--token", "secret", "--bind", "not-an-ip", "--json"]) ==
                 1
      end)

    assert %{
             "ok" => false,
             "error" => %{"code" => "coordinator.invalid_bind"}
           } = Jason.decode!(output)
  end
end
