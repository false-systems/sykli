defmodule Sykli.TeamCoordinator.RunClient do
  @moduledoc "Thin run-summary adapter over the coordinator HTTP API."

  alias Sykli.Coordinator.Client
  alias Sykli.TeamCoordinator.RunSummary

  def publish(session, token, %RunSummary{} = summary, opts \\ []) do
    publish_raw(
      session,
      token,
      RunSummary.encode(summary, secrets: Keyword.get(opts, :secrets, [])),
      opts
    )
  end

  def publish_raw(session, token, payload, opts \\ []) when is_map(payload) do
    session
    |> post_json("/v1/runs", token, payload, opts)
    |> unwrap("run")
  end

  def list(session, token, filters \\ %{}, opts \\ []) do
    path =
      case filters |> Enum.reject(fn {_k, v} -> v in [nil, ""] end) |> Map.new() do
        empty when empty == %{} -> "/v1/runs"
        query -> "/v1/runs?" <> URI.encode_query(query)
      end

    session
    |> get_json(path, token, opts)
    |> unwrap("items")
  end

  def show(session, token, run_id, opts \\ []) do
    session
    |> get_json("/v1/runs/#{run_id}", token, opts)
    |> unwrap("run")
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

  defp unwrap({:error, {:coordinator_error, 413, _error}}, _key),
    do: {:error, :team_run_body_too_large}

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
