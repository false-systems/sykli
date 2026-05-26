defmodule Sykli.TeamCoordinator.RouterTest do
  use ExUnit.Case, async: true

  import Plug.Conn
  import Plug.Test

  alias Sykli.TeamCoordinator.{Auth, Router, Store}

  @token "test-token"
  @now "2026-05-09T10:00:00Z"

  setup do
    {:ok, store} =
      Store.start_link(
        now: fn -> @now end,
        id:
          id_sequence(
            ~w(org_001 audit_001 team_001 audit_002 work_001 audit_003 audit_004 note_001 audit_005 sess_001 audit_006 audit_007)
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

  test "creates lists shows and heartbeats daemon sessions", %{opts: opts} do
    assert 201 =
             :post
             |> authed_json_conn("/v1/orgs", %{
               "slug" => "false-systems",
               "name" => "False Systems"
             })
             |> call(opts)
             |> Map.fetch!(:status)

    assert 201 =
             :post
             |> authed_json_conn("/v1/teams", %{
               "org_id" => "org_001",
               "slug" => "platform",
               "name" => "Platform"
             })
             |> call(opts)
             |> Map.fetch!(:status)

    join =
      :post
      |> authed_json_conn("/v1/daemon-sessions", %{
        "daemon_id" => "yair-mbp",
        "org" => "false-systems",
        "team" => "platform",
        "labels" => ["macos", "docker"],
        "capabilities" => ["local", "shell"],
        "version" => "0.6.1"
      })
      |> call(opts)

    assert join.status == 201
    session_id = json(join)["data"]["session_id"]
    assert is_binary(session_id)
    assert json(join)["data"]["team_id"] == "team_001"
    assert json(join)["data"]["heartbeat_interval_seconds"] == 15
    assert json(join)["data"]["policy"]["upload_raw_logs_by_default"] == false

    list =
      :get
      |> authed_conn("/v1/daemon-sessions")
      |> call(opts)

    assert list.status == 200
    assert [%{"id" => ^session_id, "accepts_remote_work" => false}] = json(list)["data"]["items"]

    show =
      :get
      |> authed_conn("/v1/daemon-sessions/#{session_id}")
      |> call(opts)

    assert show.status == 200
    assert json(show)["data"]["daemon_session"]["status"] == "available"

    heartbeat =
      :post
      |> authed_json_conn("/v1/daemon-sessions/#{session_id}/heartbeat", %{
        "session_id" => session_id,
        "status" => "busy",
        "labels" => ["macos"],
        "capabilities" => ["local"],
        "last_run_id" => "run_001"
      })
      |> call(opts)

    assert heartbeat.status == 200
    assert json(heartbeat)["data"]["next_heartbeat_seconds"] == 15
    assert json(heartbeat)["data"]["assignments"] == []

    invalid =
      :post
      |> authed_json_conn("/v1/daemon-sessions/#{session_id}/heartbeat", %{"status" => "wat"})
      |> call(opts)

    assert invalid.status == 400
    assert json(invalid)["error"]["code"] == "coordinator.invalid_daemon_status"
  end

  test "records lists and shows runs through JSON API", %{opts: opts} do
    :post
    |> authed_json_conn("/v1/orgs", %{"slug" => "false-systems", "name" => "False Systems"})
    |> call(opts)

    :post
    |> authed_json_conn("/v1/teams", %{
      "org_id" => "org_001",
      "slug" => "platform",
      "name" => "Platform"
    })
    |> call(opts)

    payload = %{
      "version" => "1",
      "run" => %{
        "id" => "run_001",
        "org_slug" => "false-systems",
        "team_slug" => "platform",
        "status" => "passed"
      },
      "nodes" => [%{"name" => "test", "kind" => "task", "status" => "passed"}],
      "criteria_results" => [],
      "review_results" => [],
      "gates" => [],
      "evidence_refs" => []
    }

    created = :post |> authed_json_conn("/v1/runs", payload) |> call(opts)
    assert created.status == 201
    assert json(created)["data"]["run"]["run"]["id"] == "run_001"

    duplicate = :post |> authed_json_conn("/v1/runs", payload) |> call(opts)
    assert duplicate.status == 200

    list = :get |> authed_conn("/v1/runs?team_id=team_001&status=passed") |> call(opts)
    assert [%{"id" => "run_001"}] = json(list)["data"]["items"]

    show = :get |> authed_conn("/v1/runs/run_001") |> call(opts)
    assert [%{"name" => "test"}] = json(show)["data"]["nodes"]
  end

  test "team tokens are scoped to their team for run endpoints" do
    {:ok, store} =
      Store.start_link(
        now: fn -> @now end,
        id:
          id_sequence(
            ~w(org_001 audit_001 team_001 audit_002 team_002 audit_003 audit_004 audit_005)
          )
      )

    opts = [store: store, token: @token]

    {:ok, _org} = Store.create_org(store, %{"slug" => "false-systems", "name" => "False Systems"})

    {:ok, _team} =
      Store.create_team(store, %{
        "org_id" => "org_001",
        "slug" => "platform",
        "name" => "Platform"
      })

    {:ok, _other} =
      Store.create_team(store, %{"org_id" => "org_001", "slug" => "infra", "name" => "Infra"})

    {:ok, _run, :inserted} = Store.record_run(store, run_payload("run_platform", "platform"))
    {:ok, _run, :inserted} = Store.record_run(store, run_payload("run_infra", "infra"))

    {:ok, token} =
      Auth.mint_team_token(%{"org" => "false-systems", "team" => "platform", "role" => "member"},
        token: @token
      )

    own_list = :get |> authed_conn("/v1/runs?team_id=team_001", token) |> call(opts)
    assert own_list.status == 200
    assert [%{"id" => "run_platform"}] = json(own_list)["data"]["items"]

    cross_list = :get |> authed_conn("/v1/runs?team_id=team_002", token) |> call(opts)
    assert cross_list.status == 403
    assert json(cross_list)["error"]["code"] == "coordinator.forbidden"

    cross_show = :get |> authed_conn("/v1/runs/run_infra", token) |> call(opts)
    assert cross_show.status == 403
    assert json(cross_show)["error"]["code"] == "coordinator.forbidden"

    admin_list = :get |> authed_conn("/v1/runs?team_id=team_002") |> call(opts)
    assert admin_list.status == 200
    assert [%{"id" => "run_infra"}] = json(admin_list)["data"]["items"]
  end

  test "team tokens cannot read other team work gates or daemon sessions" do
    {:ok, store} =
      Store.start_link(
        now: fn -> @now end,
        id:
          id_sequence(
            ~w(org_001 audit_001 team_001 audit_002 team_002 audit_003 work_001 audit_004 work_002 audit_005 sess_001 audit_006 sess_002 audit_007 audit_008 audit_009)
          )
      )

    opts = [store: store, token: @token]
    setup_two_teams!(store)

    {:ok, _work} =
      Store.create_work_item(store, %{
        "org_id" => "org_001",
        "team_id" => "team_001",
        "title" => "Platform work"
      })

    {:ok, _other_work} =
      Store.create_work_item(store, %{
        "org_id" => "org_001",
        "team_id" => "team_002",
        "title" => "Infra work"
      })

    {:ok, _join, platform_session} =
      Store.create_daemon_session(store, daemon_payload("daemon-platform", "platform"))

    {:ok, _join, infra_session} =
      Store.create_daemon_session(store, daemon_payload("daemon-infra", "infra"))

    {:ok, _gate, :inserted} =
      Store.upsert_gate(
        store,
        gate_payload("gate_platform", platform_session["session_id"], "platform")
      )

    {:ok, _gate, :inserted} =
      Store.upsert_gate(store, gate_payload("gate_infra", infra_session["session_id"], "infra"))

    {:ok, token} =
      Auth.mint_team_token(%{"org" => "false-systems", "team" => "platform", "role" => "member"},
        token: @token
      )

    for path <- [
          "/v1/work-items?team_id=team_002",
          "/v1/work-items/work_002",
          "/v1/daemon-sessions?team_id=team_002",
          "/v1/daemon-sessions/sess_002",
          "/v1/gates?team_id=team_002",
          "/v1/gates/gate_infra"
        ] do
      conn = :get |> authed_conn(path, token) |> call(opts)
      assert conn.status == 403
      assert json(conn)["error"]["code"] == "coordinator.forbidden"
    end

    member_decision =
      :post
      |> authed_json_conn(
        "/v1/gates/gate_platform/decisions",
        gate_decision_payload("platform"),
        token
      )
      |> call(opts)

    assert member_decision.status == 403
    assert json(member_decision)["error"]["code"] == "coordinator.forbidden"
  end

  test "publishes lists and decides gates through JSON API", %{opts: opts} do
    :post
    |> authed_json_conn("/v1/orgs", %{"slug" => "false-systems", "name" => "False Systems"})
    |> call(opts)

    :post
    |> authed_json_conn("/v1/teams", %{
      "org_id" => "org_001",
      "slug" => "platform",
      "name" => "Platform"
    })
    |> call(opts)

    join =
      :post
      |> authed_json_conn("/v1/daemon-sessions", %{
        "daemon_id" => "daemon-x",
        "org" => "false-systems",
        "team" => "platform",
        "labels" => ["local"],
        "capabilities" => ["local"],
        "version" => "0.6.1"
      })
      |> call(opts)

    session_id = json(join)["data"]["session_id"]

    created =
      :post
      |> authed_json_conn("/v1/gates", %{
        "org_slug" => "false-systems",
        "team_slug" => "platform",
        "daemon_session_id" => session_id,
        "id" => "gate_001",
        "run_id" => "run_001",
        "work_item_id" => nil,
        "status" => "waiting",
        "decided_by" => nil,
        "decided_at" => nil,
        "reason" => nil
      })
      |> call(opts)

    assert created.status == 201
    assert json(created)["data"]["gate"]["id"] == "gate_001"

    list =
      :get |> authed_conn("/v1/gates?org_slug=false-systems&team_slug=platform") |> call(opts)

    assert [%{"id" => "gate_001", "status" => "waiting"}] = json(list)["data"]["items"]

    decided =
      :post
      |> authed_json_conn("/v1/gates/gate_001/decisions", %{
        "org_slug" => "false-systems",
        "team_slug" => "platform",
        "status" => "approved",
        "decided_by" => "member:reviewer",
        "decided_at" => "2026-05-09T10:05:00Z",
        "reason" => "Reviewed"
      })
      |> call(opts)

    assert decided.status == 200
    assert json(decided)["data"]["gate"]["status"] == "approved"

    heartbeat =
      :post
      |> authed_json_conn("/v1/daemon-sessions/#{session_id}/heartbeat", %{
        "session_id" => session_id,
        "status" => "available"
      })
      |> call(opts)

    assert [%{"id" => "gate_001", "status" => "approved"}] = json(heartbeat)["data"]["decisions"]
  end

  test "run endpoints return structured errors", %{opts: opts} do
    unauthorized = call(conn(:get, "/v1/runs"), opts)
    assert unauthorized.status == 401

    :post
    |> authed_json_conn("/v1/orgs", %{"slug" => "false-systems", "name" => "False Systems"})
    |> call(opts)

    :post
    |> authed_json_conn("/v1/teams", %{
      "org_id" => "org_001",
      "slug" => "platform",
      "name" => "Platform"
    })
    |> call(opts)

    invalid =
      :post
      |> authed_json_conn("/v1/runs", %{
        "version" => "1",
        "run" => %{
          "id" => "run_bad",
          "org_slug" => "false-systems",
          "team_slug" => "platform",
          "status" => "wat"
        }
      })
      |> call(opts)

    assert invalid.status == 400
    assert json(invalid)["error"]["code"] == "team.run.invalid_payload"

    missing = :get |> authed_conn("/v1/runs/missing") |> call(opts)
    assert missing.status == 404
  end

  defp call(conn, opts) do
    conn
    |> fetch_query_params()
    |> Router.call(opts)
  end

  defp authed_conn(method, path) do
    authed_conn(method, path, @token)
  end

  defp authed_conn(method, path, token) do
    method
    |> conn(path)
    |> put_req_header("authorization", "Bearer #{token}")
  end

  defp json_conn(method, path, body) do
    method
    |> conn(path, Jason.encode!(body))
    |> put_req_header("content-type", "application/json")
  end

  defp authed_json_conn(method, path, body) do
    authed_json_conn(method, path, body, @token)
  end

  defp authed_json_conn(method, path, body, token) do
    method
    |> json_conn(path, body)
    |> put_req_header("authorization", "Bearer #{token}")
  end

  defp setup_two_teams!(store) do
    {:ok, _org} = Store.create_org(store, %{"slug" => "false-systems", "name" => "False Systems"})

    {:ok, _team} =
      Store.create_team(store, %{
        "org_id" => "org_001",
        "slug" => "platform",
        "name" => "Platform"
      })

    {:ok, _other} =
      Store.create_team(store, %{"org_id" => "org_001", "slug" => "infra", "name" => "Infra"})
  end

  defp run_payload(id, team_slug) do
    %{
      "version" => "1",
      "run" => %{
        "id" => id,
        "org_slug" => "false-systems",
        "team_slug" => team_slug,
        "status" => "passed"
      },
      "nodes" => [],
      "criteria_results" => [],
      "review_results" => [],
      "gates" => [],
      "evidence_refs" => []
    }
  end

  defp daemon_payload(id, team) do
    %{
      "daemon_id" => id,
      "org" => "false-systems",
      "team" => team,
      "labels" => ["local"],
      "capabilities" => ["local"],
      "version" => "0.6.1"
    }
  end

  defp gate_payload(id, session_id, team_slug) do
    %{
      "org_slug" => "false-systems",
      "team_slug" => team_slug,
      "daemon_session_id" => session_id,
      "id" => id,
      "run_id" => "run_#{team_slug}",
      "work_item_id" => nil,
      "status" => "waiting",
      "decided_by" => nil,
      "decided_at" => nil,
      "reason" => nil
    }
  end

  defp gate_decision_payload(team_slug) do
    %{
      "org_slug" => "false-systems",
      "team_slug" => team_slug,
      "status" => "approved",
      "decided_by" => "member:reviewer",
      "decided_at" => "2026-05-09T10:05:00Z",
      "reason" => "Reviewed"
    }
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
