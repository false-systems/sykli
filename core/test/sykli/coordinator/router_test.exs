defmodule Sykli.Coordinator.RouterTest do
  use ExUnit.Case, async: true

  import Plug.Conn
  import Plug.Test

  alias Sykli.Coordinator.{Router, Store}

  @token "test-token"
  @now "2026-05-09T10:00:00Z"

  setup do
    {:ok, store} =
      Store.start_link(
        now: fn -> @now end,
        id:
          id_sequence(
            ~w(org_001 audit_001 team_001 audit_002 work_001 audit_003 audit_004 note_001 audit_005)
          )
      )

    {:ok, opts: [store: store, token: @token]}
  end

  test "GET /health succeeds without auth", %{opts: opts} do
    conn = call(conn(:get, "/health"), opts)

    assert conn.status == 200
    assert json(conn)["data"] == %{"service" => "sykli-coordinator", "status" => "ok"}
  end

  test "non-health endpoints require a valid bearer token", %{opts: opts} do
    missing = call(conn(:post, "/v1/orgs", "{}"), opts)
    assert missing.status == 401
    assert json(missing)["error"]["code"] == "coordinator.unauthorized"

    wrong =
      :post
      |> json_conn("/v1/orgs", %{"slug" => "false-systems", "name" => "False Systems"})
      |> put_req_header("authorization", "Bearer wrong")
      |> call(opts)

    assert wrong.status == 401
    assert json(wrong)["error"]["code"] == "coordinator.unauthorized"
  end

  test "creates org team work claim and note through JSON API", %{opts: opts} do
    org =
      :post
      |> authed_json_conn("/v1/orgs", %{"slug" => "false-systems", "name" => "False Systems"})
      |> call(opts)

    assert org.status == 201
    assert json(org)["data"]["org"]["id"] == "org_001"

    team =
      :post
      |> authed_json_conn("/v1/teams", %{
        "org_id" => "org_001",
        "slug" => "platform",
        "name" => "Platform"
      })
      |> call(opts)

    assert team.status == 201
    assert json(team)["data"]["team"]["id"] == "team_001"

    work =
      :post
      |> authed_json_conn("/v1/work-items", %{
        "org_id" => "org_001",
        "team_id" => "team_001",
        "title" => "Investigate deploy"
      })
      |> call(opts)

    assert work.status == 201
    assert json(work)["data"]["work_item"]["id"] == "work_001"

    list =
      :get
      |> authed_conn("/v1/work-items")
      |> call(opts)

    assert list.status == 200
    assert [%{"id" => "work_001"}] = json(list)["data"]["items"]

    show =
      :get
      |> authed_conn("/v1/work-items/work_001")
      |> call(opts)

    assert show.status == 200
    assert json(show)["data"]["work_item"]["title"] == "Investigate deploy"

    claim =
      :post
      |> authed_json_conn("/v1/work-items/work_001/claim", %{
        "assigned_to_type" => "member",
        "assigned_to_id" => "yair"
      })
      |> call(opts)

    assert claim.status == 200
    assert json(claim)["data"]["work_item"]["status"] == "claimed"

    note =
      :post
      |> authed_json_conn("/v1/work-items/work_001/notes", %{"body" => "Found issue"})
      |> call(opts)

    assert note.status == 201
    assert json(note)["data"]["note"]["body"] == "Found issue"
  end

  test "returns structured errors for invalid JSON duplicate slug and not found", %{opts: opts} do
    invalid_json =
      :post
      |> conn("/v1/orgs", "{bad")
      |> put_req_header("authorization", "Bearer #{@token}")
      |> call(opts)

    assert invalid_json.status == 400
    assert json(invalid_json)["error"]["code"] == "coordinator.invalid_json"

    assert 201 =
             :post
             |> authed_json_conn("/v1/orgs", %{
               "slug" => "false-systems",
               "name" => "False Systems"
             })
             |> call(opts)
             |> Map.fetch!(:status)

    duplicate =
      :post
      |> authed_json_conn("/v1/orgs", %{"slug" => "false-systems", "name" => "Duplicate"})
      |> call(opts)

    assert duplicate.status == 409
    assert json(duplicate)["error"]["code"] == "coordinator.duplicate_org_slug"

    missing =
      :get
      |> authed_conn("/v1/work-items/missing")
      |> call(opts)

    assert missing.status == 404
    assert json(missing)["error"]["code"] == "work_item_not_found"
  end

  test "does not expose dangerous execution endpoints", %{opts: opts} do
    conn =
      :post
      |> authed_json_conn("/v1/execute", %{"command" => "whoami"})
      |> call(opts)

    assert conn.status == 404
    assert json(conn)["error"]["code"] == "coordinator.not_found"
  end

  defp call(conn, opts) do
    conn
    |> fetch_query_params()
    |> Router.call(opts)
  end

  defp authed_conn(method, path) do
    method
    |> conn(path)
    |> put_req_header("authorization", "Bearer #{@token}")
  end

  defp json_conn(method, path, body) do
    method
    |> conn(path, Jason.encode!(body))
    |> put_req_header("content-type", "application/json")
  end

  defp authed_json_conn(method, path, body) do
    method
    |> json_conn(path, body)
    |> put_req_header("authorization", "Bearer #{@token}")
  end

  defp json(conn), do: Jason.decode!(conn.resp_body)

  defp id_sequence(ids) do
    {:ok, agent} = Agent.start_link(fn -> ids end)

    fn ->
      Agent.get_and_update(agent, fn
        [id | rest] -> {id, rest}
        [] -> flunk("id sequence exhausted")
      end)
    end
  end
end
