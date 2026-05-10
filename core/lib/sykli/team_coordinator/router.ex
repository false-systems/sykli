defmodule Sykli.TeamCoordinator.Router do
  @moduledoc """
  Plug router for the self-hosted Team Mode coordinator skeleton.

  This HTTP surface coordinates org/team/work metadata only. It deliberately
  exposes no execution, shell, log upload, artifact upload, run, gate, or
  evidence endpoints.
  """

  import Plug.Conn

  alias Sykli.CLI.JsonResponse
  alias Sykli.TeamCoordinator.{Auth, Store}
  alias Sykli.Error

  @max_body_bytes 1_000_000
  @read_timeout_ms 15_000

  def init(opts), do: opts

  def call(%Plug.Conn{method: "GET", path_info: path} = conn, _opts)
      when path in [["health"], ["healthz"]] do
    send_json(conn, 200, %{status: "ok", service: "sykli-coordinator"})
  end

  def call(%Plug.Conn{path_info: ["v1" | _]} = conn, opts) do
    with :ok <- Auth.authorize(conn, opts) do
      route_v1(conn, opts)
    else
      {:error, reason} -> send_error(conn, reason)
    end
  end

  def call(conn, _opts), do: send_error(conn, :coordinator_route_not_found)

  defp route_v1(%Plug.Conn{method: "POST", path_info: ["v1", "orgs"]} = conn, opts) do
    with {:ok, body, conn} <- read_json(conn),
         {:ok, org} <- Store.create_org(store(opts), body) do
      send_json(conn, 201, %{org: org})
    else
      {:error, reason} -> send_error(conn, reason)
    end
  end

  defp route_v1(%Plug.Conn{method: "GET", path_info: ["v1", "orgs"]} = conn, opts) do
    with {:ok, orgs} <- Store.list_orgs(store(opts)) do
      send_json(conn, 200, %{items: orgs})
    end
  end

  defp route_v1(%Plug.Conn{method: "POST", path_info: ["v1", "teams"]} = conn, opts) do
    with {:ok, body, conn} <- read_json(conn),
         {:ok, team} <- Store.create_team(store(opts), body) do
      send_json(conn, 201, %{team: team})
    else
      {:error, reason} -> send_error(conn, reason)
    end
  end

  defp route_v1(%Plug.Conn{method: "GET", path_info: ["v1", "teams"]} = conn, opts) do
    with {:ok, teams} <- Store.list_teams(store(opts)) do
      send_json(conn, 200, %{items: teams})
    end
  end

  defp route_v1(%Plug.Conn{method: "GET", path_info: ["v1", "work-items"]} = conn, opts) do
    conn = fetch_query_params(conn)

    filters =
      conn.query_params
      |> Map.take(["org_id", "team_id"])
      |> Enum.reject(fn {_key, value} -> value in [nil, ""] end)
      |> Map.new()

    with {:ok, items} <- Store.list_work_items(store(opts), filters) do
      send_json(conn, 200, %{items: items})
    end
  end

  defp route_v1(%Plug.Conn{method: "POST", path_info: ["v1", "work-items"]} = conn, opts) do
    with {:ok, body, conn} <- read_json(conn),
         {:ok, item} <- Store.create_work_item(store(opts), body) do
      send_json(conn, 201, %{work_item: item})
    else
      {:error, reason} -> send_error(conn, reason)
    end
  end

  defp route_v1(%Plug.Conn{method: "GET", path_info: ["v1", "work-items", id]} = conn, opts) do
    with {:ok, item} <- Store.get_work_item(store(opts), id) do
      send_json(conn, 200, %{work_item: item})
    else
      {:error, reason} -> send_error(conn, reason)
    end
  end

  defp route_v1(
         %Plug.Conn{method: "POST", path_info: ["v1", "work-items", id, "claim"]} = conn,
         opts
       ) do
    with {:ok, body, conn} <- read_json(conn),
         {:ok, item} <- Store.claim_work_item(store(opts), id, body) do
      send_json(conn, 200, %{work_item: item})
    else
      {:error, reason} -> send_error(conn, reason)
    end
  end

  defp route_v1(
         %Plug.Conn{method: "POST", path_info: ["v1", "work-items", id, "notes"]} = conn,
         opts
       ) do
    with {:ok, body, conn} <- read_json(conn),
         {:ok, note} <- Store.add_note(store(opts), id, body) do
      send_json(conn, 201, %{note: note})
    else
      {:error, reason} -> send_error(conn, reason)
    end
  end

  defp route_v1(%Plug.Conn{method: "POST", path_info: ["v1", "daemon-sessions"]} = conn, opts) do
    with {:ok, body, conn} <- read_json(conn),
         {:ok, response, _session} <- Store.create_daemon_session(store(opts), body) do
      send_json(conn, 201, response)
    else
      {:error, reason} -> send_error(conn, reason)
    end
  end

  defp route_v1(%Plug.Conn{method: "GET", path_info: ["v1", "daemon-sessions"]} = conn, opts) do
    conn = fetch_query_params(conn)

    filters =
      conn.query_params
      |> Map.take(["org_id", "team_id"])
      |> Enum.reject(fn {_key, value} -> value in [nil, ""] end)
      |> Map.new()

    with {:ok, sessions} <- Store.list_daemon_sessions(store(opts), filters) do
      send_json(conn, 200, %{items: sessions})
    end
  end

  defp route_v1(%Plug.Conn{method: "GET", path_info: ["v1", "daemon-sessions", id]} = conn, opts) do
    with {:ok, session} <- Store.get_daemon_session(store(opts), id) do
      send_json(conn, 200, %{daemon_session: session})
    else
      {:error, reason} -> send_error(conn, reason)
    end
  end

  defp route_v1(
         %Plug.Conn{method: "POST", path_info: ["v1", "daemon-sessions", id, "heartbeat"]} =
           conn,
         opts
       ) do
    with {:ok, body, conn} <- read_json(conn),
         {:ok, heartbeat, _session} <- Store.heartbeat_daemon_session(store(opts), id, body) do
      send_json(conn, 200, heartbeat)
    else
      {:error, reason} -> send_error(conn, reason)
    end
  end

  defp route_v1(conn, _opts), do: send_error(conn, :coordinator_route_not_found)

  defp read_json(conn) do
    case read_body(conn, length: @max_body_bytes, read_timeout: @read_timeout_ms) do
      {:ok, body, conn} ->
        case Jason.decode(body) do
          {:ok, value} when is_map(value) -> {:ok, value, conn}
          {:ok, _value} -> {:error, :coordinator_invalid_payload}
          {:error, _error} -> {:error, :coordinator_invalid_json}
        end

      {:more, _partial, _conn} ->
        {:error, :coordinator_payload_too_large}

      {:error, _reason} ->
        {:error, :coordinator_body_read_failed}
    end
  end

  defp send_json(conn, status, data) do
    conn
    |> put_resp_content_type("application/json")
    |> send_resp(status, JsonResponse.ok(data))
  end

  defp send_error(conn, reason) do
    error = coordinator_error(reason)

    conn
    |> put_resp_content_type("application/json")
    |> send_resp(status_for(reason), JsonResponse.error(error))
  end

  defp store(opts), do: Keyword.fetch!(opts, :store)

  defp status_for(:coordinator_unauthorized), do: 401
  defp status_for(:coordinator_malformed_auth), do: 401
  defp status_for(:coordinator_auth_not_configured), do: 503
  defp status_for(:coordinator_invalid_json), do: 400
  defp status_for(:coordinator_invalid_payload), do: 400
  defp status_for(:coordinator_payload_too_large), do: 413
  defp status_for(:coordinator_body_read_failed), do: 408
  defp status_for(:coordinator_route_not_found), do: 404
  defp status_for({:missing_field, _field}), do: 400
  defp status_for({:invalid_field, _field}), do: 400
  defp status_for({:duplicate_org_slug, _slug}), do: 409
  defp status_for({:duplicate_team_slug, _slug}), do: 409
  defp status_for({:org_not_found, _org}), do: 404
  defp status_for({:team_not_found, _team}), do: 404
  defp status_for({:work_item_not_found, _id}), do: 404
  defp status_for({:invalid_assignment_type, _type}), do: 400
  defp status_for({:invalid_work_item_id, _id}), do: 400
  defp status_for({:work_item_already_claimed, _id, _assignment}), do: 409
  defp status_for({:invalid_daemon_id, _id}), do: 400
  defp status_for({:invalid_daemon_list_field, _field}), do: 400
  defp status_for({:invalid_daemon_status, _status}), do: 400
  defp status_for({:invalid_daemon_session_id, _id}), do: 400
  defp status_for({:daemon_session_not_found, _id}), do: 404
  defp status_for(_reason), do: 500

  defp coordinator_error(:coordinator_unauthorized),
    do: error("coordinator.unauthorized", "authorization required")

  defp coordinator_error(:coordinator_malformed_auth),
    do: error("coordinator.unauthorized", "authorization must use Bearer token")

  defp coordinator_error(:coordinator_auth_not_configured),
    do: error("coordinator.auth_not_configured", "coordinator auth token is not configured")

  defp coordinator_error(:coordinator_invalid_json),
    do: error("coordinator.invalid_json", "request body is not valid JSON")

  defp coordinator_error(:coordinator_invalid_payload),
    do: error("coordinator.invalid_payload", "request body must be a JSON object")

  defp coordinator_error(:coordinator_payload_too_large),
    do: error("coordinator.payload_too_large", "request body exceeds coordinator limit")

  defp coordinator_error(:coordinator_body_read_failed),
    do: error("coordinator.body_read_failed", "failed to read request body")

  defp coordinator_error(:coordinator_route_not_found),
    do: error("coordinator.not_found", "coordinator endpoint was not found")

  defp coordinator_error({:missing_field, field}),
    do: error("coordinator.invalid_payload", "missing required field: #{field}")

  defp coordinator_error({:invalid_field, field}),
    do: error("coordinator.invalid_payload", "invalid field: #{field}")

  defp coordinator_error({:duplicate_org_slug, slug}),
    do: error("coordinator.duplicate_org_slug", "org slug already exists: #{slug}")

  defp coordinator_error({:duplicate_team_slug, slug}),
    do: error("coordinator.duplicate_team_slug", "team slug already exists: #{slug}")

  defp coordinator_error({:org_not_found, org}),
    do: error("coordinator.org_not_found", "org was not found: #{inspect(org)}")

  defp coordinator_error({:team_not_found, team}),
    do: error("coordinator.team_not_found", "team was not found: #{inspect(team)}")

  defp coordinator_error({:work_item_not_found, id}), do: Error.work_item_not_found(id)
  defp coordinator_error({:invalid_work_item_id, id}), do: Error.invalid_work_item_id(id)

  defp coordinator_error({:invalid_assignment_type, type}),
    do: error("coordinator.invalid_assignment_type", "invalid assignment type: #{inspect(type)}")

  defp coordinator_error({:work_item_already_claimed, id, assignment}),
    do: Error.work_item_already_claimed(id, assignment)

  defp coordinator_error({:invalid_daemon_id, id}),
    do: error("coordinator.invalid_daemon_id", "invalid daemon id: #{inspect(id)}")

  defp coordinator_error({:invalid_daemon_list_field, field}),
    do:
      error(
        "coordinator.invalid_daemon_payload",
        "daemon field must be a list of non-empty strings: #{field}"
      )

  defp coordinator_error({:invalid_daemon_status, status}),
    do: error("coordinator.invalid_daemon_status", "invalid daemon status: #{inspect(status)}")

  defp coordinator_error({:invalid_daemon_session_id, id}),
    do:
      error("coordinator.invalid_daemon_session_id", "invalid daemon session id: #{inspect(id)}")

  defp coordinator_error({:daemon_session_not_found, id}),
    do: error("coordinator.daemon_session_not_found", "daemon session was not found: #{id}")

  defp coordinator_error(reason),
    do: error("coordinator.internal_error", "coordinator failed: #{inspect(reason)}")

  defp error(code, message) do
    %Error{
      code: code,
      type: :runtime,
      message: message,
      step: :setup,
      hints: []
    }
  end
end
