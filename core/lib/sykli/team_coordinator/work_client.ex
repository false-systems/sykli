defmodule Sykli.TeamCoordinator.WorkClient do
  @moduledoc """
  Client helpers for Team Mode work-item operations.

  This module is intentionally a thin adapter over the coordinator HTTP API.
  It does not cache team work locally and it does not execute or assign work.
  """

  alias Sykli.Coordinator.Client
  alias Sykli.WorkItem

  def create(session, token, attrs, opts \\ []) do
    body =
      attrs
      |> Map.take(["title", "intent", "created_by"])
      |> Map.put("org_slug", session["org"])
      |> Map.put("team_slug", session["team"])

    session
    |> post_json("/v1/work-items", token, body, opts)
    |> unwrap("work_item")
  end

  def list(session, token, opts \\ []) do
    session
    |> get_json(work_items_path(session), token, opts)
    |> unwrap("items")
  end

  def show(session, token, id, opts \\ []) do
    with :ok <- WorkItem.validate_id(id) do
      session
      |> get_json("/v1/work-items/#{id}", token, opts)
      |> unwrap("work_item")
    end
  end

  def claim(session, token, id, attrs, opts \\ []) do
    with :ok <- WorkItem.validate_id(id) do
      session
      |> post_json("/v1/work-items/#{id}/claim", token, attrs, opts)
      |> unwrap("work_item")
    end
  end

  def note(session, token, id, attrs, opts \\ []) do
    with :ok <- WorkItem.validate_id(id) do
      session
      |> post_json("/v1/work-items/#{id}/notes", token, attrs, opts)
      |> unwrap("note")
    end
  end

  defp work_items_path(%{"team_id" => team_id}) when is_binary(team_id) and team_id != "" do
    "/v1/work-items?" <> URI.encode_query(%{"team_id" => team_id})
  end

  defp work_items_path(_session), do: "/v1/work-items"

  defp get_json(session, path, token, opts) do
    client(opts).get_json(session["coordinator"], path, token)
  end

  defp post_json(session, path, token, body, opts) do
    client(opts).post_json(session["coordinator"], path, token, body)
  end

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
