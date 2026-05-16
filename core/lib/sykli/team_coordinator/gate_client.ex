defmodule Sykli.TeamCoordinator.GateClient do
  @moduledoc "Thin gate-decision adapter over the coordinator HTTP API."

  alias Sykli.Coordinator.Client
  alias Sykli.GateDecision
  alias Sykli.TeamCoordinator.GateDecisionSummary

  def publish_waiting(session, token, %GateDecisionSummary{} = summary, opts \\ []) do
    body =
      summary
      |> GateDecisionSummary.encode()
      |> Map.put("org_slug", session["org"] || session["org_slug"])
      |> Map.put("team_slug", session["team"] || session["team_slug"])
      |> Map.put("daemon_session_id", session["session_id"] || session["daemon_session_id"])

    session
    |> post_json("/v1/gates", token, body, opts)
    |> unwrap("gate")
  end

  def publish_raw(session, token, payload, opts \\ []) when is_map(payload) do
    session
    |> post_json("/v1/gates", token, payload, opts)
    |> unwrap("gate")
  end

  def record_decision(session, token, id, decision, opts \\ []) do
    with :ok <- GateDecision.validate_id(id) do
      body =
        decision
        |> Map.take(["status", "decided_by", "decided_at", "reason"])
        |> Map.put("org_slug", session["org"] || session["org_slug"])
        |> Map.put("team_slug", session["team"] || session["team_slug"])

      session
      |> post_json("/v1/gates/#{id}/decisions", token, body, opts)
      |> unwrap("gate")
    end
  end

  def list(session, token, opts \\ []) do
    session
    |> get_json(gates_path(session), token, opts)
    |> unwrap("items")
  end

  def get(session, token, id, opts \\ []) do
    with :ok <- GateDecision.validate_id(id) do
      session
      |> get_json("/v1/gates/#{id}", token, opts)
      |> unwrap("gate")
    end
  end

  defp gates_path(session) do
    query =
      %{
        "org_slug" => session["org"] || session["org_slug"],
        "team_slug" => session["team"] || session["team_slug"],
        "team_id" => session["team_id"]
      }
      |> Enum.reject(fn {_key, value} -> value in [nil, ""] end)
      |> Map.new()

    case query do
      empty when empty == %{} -> "/v1/gates"
      query -> "/v1/gates?" <> URI.encode_query(query)
    end
  end

  defp get_json(session, path, token, opts),
    do: client(opts).get_json(session["coordinator"], path, token)

  defp post_json(session, path, token, body, opts),
    do: client(opts).post_json(session["coordinator"], path, token, body)

  defp unwrap({:ok, data}, key) do
    case Map.fetch(data, key) do
      {:ok, value} -> {:ok, value}
      :error -> {:error, {:team_invalid_coordinator_response, data}}
    end
  end

  defp unwrap({:error, {:coordinator_error, 401, _error}}, _key), do: {:error, :team_unauthorized}

  defp unwrap({:error, {:coordinator_error, _status, error}}, _key),
    do: {:error, {:team_coordinator_error, error}}

  defp unwrap({:error, {:coordinator_unavailable, reason}}, _key),
    do: {:error, {:team_coordinator_unavailable, reason}}

  defp unwrap({:error, :invalid_coordinator_response}, _key),
    do: {:error, :team_invalid_coordinator_response}

  defp unwrap({:error, {:invalid_coordinator_response, data}}, _key),
    do: {:error, {:team_invalid_coordinator_response, data}}

  defp unwrap({:error, reason}, _key), do: {:error, reason}

  defp client(opts), do: Keyword.get(opts, :client, Client)
end
