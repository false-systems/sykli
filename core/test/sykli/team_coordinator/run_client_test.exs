defmodule Sykli.TeamCoordinator.RunClientTest do
  use ExUnit.Case, async: true

  alias Sykli.TeamCoordinator.{RunClient, RunSummary}

  @session %{"coordinator" => "https://sykli.internal", "team_id" => "team_001"}
  @payload %{
    "version" => "1",
    "run" => %{"id" => "run_001", "status" => "passed"},
    "nodes" => [],
    "criteria_results" => [],
    "review_results" => [],
    "gates" => [],
    "evidence_refs" => []
  }

  test "publish posts encoded summary" do
    summary = struct!(RunSummary, run: @payload["run"])

    assert {:ok, %{"id" => "run_001"}} =
             RunClient.publish(@session, "secret", summary, client: __MODULE__.FakeClient)

    assert_received {:post, "https://sykli.internal", "/v1/runs", "secret", %{"run" => _}}
  end

  test "publish masks explicit secrets in encoded summary" do
    summary =
      struct!(RunSummary,
        run: %{"id" => "run_001", "status" => "failed", "error_code" => "token leaked-value"},
        nodes: [%{"name" => "build", "message" => "leaked-value"}]
      )

    assert {:ok, %{"id" => "run_001"}} =
             RunClient.publish(@session, "secret", summary,
               client: __MODULE__.FakeClient,
               secrets: ["leaked-value"]
             )

    assert_received {:post, _, _, _, body}
    assert body["run"]["error_code"] == "token ***MASKED***"
    assert body["nodes"] == [%{"name" => "build", "message" => "***MASKED***"}]
  end

  test "publish_raw list and show use run endpoints" do
    assert {:ok, %{"id" => "run_001"}} =
             RunClient.publish_raw(@session, "secret", @payload, client: __MODULE__.FakeClient)

    assert {:ok, [%{"id" => "run_001"}]} =
             RunClient.list(@session, "secret", %{"team_id" => "team_001"},
               client: __MODULE__.FakeClient
             )

    assert {:ok, %{"id" => "run_001"}} =
             RunClient.show(@session, "secret", "run_001", client: __MODULE__.FakeClient)

    assert_received {:get, _, "/v1/runs?team_id=team_001", "secret"}
    assert_received {:get, _, "/v1/runs/run_001", "secret"}
  end

  test "maps unavailable and unauthorized" do
    assert {:error, :team_unauthorized} =
             RunClient.list(@session, "secret", %{}, client: __MODULE__.UnauthorizedClient)

    assert {:error, {:team_coordinator_unavailable, :econnrefused}} =
             RunClient.list(@session, "secret", %{}, client: __MODULE__.UnavailableClient)
  end

  defmodule FakeClient do
    def post_json(base, path, token, body) do
      send(self(), {:post, base, path, token, body})
      {:ok, %{"run" => %{"id" => "run_001"}}}
    end

    def get_json(base, path, token) do
      send(self(), {:get, base, path, token})

      case path do
        "/v1/runs?team_id=team_001" -> {:ok, %{"items" => [%{"id" => "run_001"}]}}
        "/v1/runs/run_001" -> {:ok, %{"run" => %{"id" => "run_001"}}}
      end
    end
  end

  defmodule UnauthorizedClient do
    def get_json(_base, _path, _token),
      do: {:error, {:coordinator_error, 401, %{"code" => "coordinator.unauthorized"}}}
  end

  defmodule UnavailableClient do
    def get_json(_base, _path, _token), do: {:error, {:coordinator_unavailable, :econnrefused}}
  end
end
