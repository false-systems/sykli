defmodule Sykli.CLI.CoordinatorTest do
  use ExUnit.Case, async: true

  import ExUnit.CaptureIO

  alias Sykli.CLI.Coordinator
  alias Sykli.TeamCoordinator.Auth

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

  test "mint-token emits an admin-signed team token as JSON" do
    output =
      capture_io(fn ->
        assert Coordinator.run([
                 "mint-token",
                 "--token",
                 "admin-secret",
                 "--org",
                 "false-systems",
                 "--team",
                 "platform",
                 "--role",
                 "approver",
                 "--json"
               ]) == 0
      end)

    decoded = Jason.decode!(output)
    token = decoded["data"]["token"]

    assert decoded["data"]["role"] == "approver"
    assert {:ok, principal} = Auth.verify_team_token(token, "admin-secret")
    assert principal.org == "false-systems"
    assert principal.team == "platform"
    assert principal.role == "approver"
  end
end
