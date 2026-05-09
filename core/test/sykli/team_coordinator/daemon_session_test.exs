defmodule Sykli.TeamCoordinator.DaemonSessionTest do
  use ExUnit.Case, async: true

  alias Sykli.TeamCoordinator.Store

  @now "2026-05-09T10:00:00Z"
  @later "2026-05-09T10:00:15Z"

  setup do
    {:ok, clock} = Agent.start_link(fn -> [@now, @later] end)

    {:ok, store} =
      Store.start_link(
        now: fn ->
          Agent.get_and_update(clock, fn
            [value | rest] -> {value, rest}
            [] -> {@later, []}
          end)
        end,
        id:
          id_sequence(
            ~w(org_001 audit_001 team_001 audit_002 sess_001 audit_003 sess_002 audit_004 audit_005)
          )
      )

    {:ok, org} = Store.create_org(store, %{"slug" => "false-systems", "name" => "False Systems"})

    {:ok, team} =
      Store.create_team(store, %{
        "org_id" => org["id"],
        "slug" => "platform",
        "name" => "Platform"
      })

    {:ok, store: store, org: org, team: team}
  end

  test "creates daemon session with safe defaults", %{store: store, team: team} do
    assert {:ok, response, session} =
             Store.create_daemon_session(store, %{
               "daemon_id" => "yair-mbp",
               "org" => "false-systems",
               "team" => "platform",
               "labels" => ["macos", "docker"],
               "capabilities" => ["local"],
               "version" => "0.6.1"
             })

    assert response["session_id"] == "sess_001"
    assert response["team_id"] == team["id"]
    assert response["heartbeat_interval_seconds"] == 15
    assert response["policy"]["upload_raw_logs_by_default"] == false
    assert session["accepts_remote_work"] == false
    assert session["status"] == "available"
    assert session["labels"] == ["macos", "docker"]
    refute Map.has_key?(session, "token")
  end

  test "rejoin supersedes prior session deterministically", %{store: store} do
    attrs = %{
      "daemon_id" => "worker-1",
      "org" => "false-systems",
      "team" => "platform",
      "labels" => [],
      "capabilities" => ["local"],
      "version" => "0.6.1"
    }

    assert {:ok, _response, first} = Store.create_daemon_session(store, attrs)
    assert {:ok, _response, second} = Store.create_daemon_session(store, attrs)

    assert {:ok, old} = Store.get_daemon_session(store, first["id"])
    assert old["status"] == "offline"
    assert old["superseded_by"] == second["id"]
  end

  test "heartbeat updates status and liveness fields", %{store: store} do
    {:ok, _response, session} =
      Store.create_daemon_session(store, %{
        "daemon_id" => "worker-1",
        "org" => "false-systems",
        "team" => "platform",
        "labels" => ["macos"],
        "capabilities" => ["local"],
        "version" => "0.6.1"
      })

    assert {:ok, heartbeat, updated} =
             Store.heartbeat_daemon_session(store, session["id"], %{
               "session_id" => session["id"],
               "status" => "busy",
               "labels" => ["macos", "docker"],
               "capabilities" => ["local", "shell"],
               "current_work_item_id" => "work_001",
               "last_run_id" => "run_001"
             })

    assert heartbeat == %{"next_heartbeat_seconds" => 15, "decisions" => [], "assignments" => []}
    assert updated["status"] == "busy"
    assert updated["labels"] == ["macos", "docker"]
    assert updated["last_seen_at"] == @later
    assert updated["current_work_item_id"] == "work_001"
  end

  test "validates daemon session inputs", %{store: store} do
    assert {:error, {:org_not_found, "missing"}} =
             Store.create_daemon_session(store, %{
               "daemon_id" => "worker-1",
               "org" => "missing",
               "team" => "platform",
               "version" => "0.6.1"
             })

    assert {:error, {:invalid_daemon_id, "../escape"}} =
             Store.create_daemon_session(store, %{
               "daemon_id" => "../escape",
               "org" => "false-systems",
               "team" => "platform",
               "version" => "0.6.1"
             })

    {:ok, _response, session} =
      Store.create_daemon_session(store, %{
        "daemon_id" => "worker-1",
        "org" => "false-systems",
        "team" => "platform",
        "version" => "0.6.1"
      })

    assert {:error, {:invalid_daemon_status, "wat"}} =
             Store.heartbeat_daemon_session(store, session["id"], %{"status" => "wat"})

    assert {:error, {:daemon_session_not_found, "missing"}} =
             Store.get_daemon_session(store, "missing")
  end

  test "lists sessions deterministically", %{store: store} do
    for daemon_id <- ["worker-b", "worker-a"] do
      assert {:ok, _response, _session} =
               Store.create_daemon_session(store, %{
                 "daemon_id" => daemon_id,
                 "org" => "false-systems",
                 "team" => "platform",
                 "version" => "0.6.1"
               })
    end

    assert {:ok, sessions} = Store.list_daemon_sessions(store)
    assert Enum.map(sessions, & &1["id"]) == ["sess_001", "sess_002"]
  end

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
