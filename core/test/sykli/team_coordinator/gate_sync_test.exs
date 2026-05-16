defmodule Sykli.TeamCoordinator.GateSyncTest do
  use ExUnit.Case, async: false

  alias Sykli.Daemon.Join
  alias Sykli.Daemon.SessionStore
  alias Sykli.Executor
  alias Sykli.Gate.Store, as: GateStore
  alias Sykli.Occurrence.PubSub
  alias Sykli.TeamCoordinator.Store

  @now "2026-05-09T10:00:00Z"
  @later "2026-05-09T10:05:00Z"

  setup do
    {:ok, store} =
      Store.start_link(
        now: fn -> @now end,
        id:
          id_sequence(
            ~w(org_001 audit_001 team_001 audit_002 team_002 audit_003 sess_x audit_004 sess_y audit_005 sess_other audit_006 gate_audit_001 decision_audit_001 hb_audit_001 hb_audit_002 hb_audit_003 sess_new audit_007 hb_audit_004)
          )
      )

    {:ok, org} = Store.create_org(store, %{"slug" => "false-systems", "name" => "False Systems"})

    {:ok, platform} =
      Store.create_team(store, %{
        "org_id" => org["id"],
        "slug" => "platform",
        "name" => "Platform"
      })

    {:ok, ops} =
      Store.create_team(store, %{"org_id" => org["id"], "slug" => "ops", "name" => "Ops"})

    {:ok, _response, daemon_x} = daemon_session(store, "daemon-x", "platform")
    {:ok, _response, daemon_y} = daemon_session(store, "daemon-y", "platform")
    {:ok, _response, daemon_other} = daemon_session(store, "daemon-other", "ops")

    {:ok,
     store: store,
     platform: platform,
     ops: ops,
     daemon_x: daemon_x,
     daemon_y: daemon_y,
     daemon_other: daemon_other}
  end

  test "cross-daemon approval roundtrip updates local file and audit log", %{
    store: store,
    daemon_x: daemon_x,
    daemon_y: daemon_y
  } do
    tmp = tmp_dir()

    assert {:ok, _local_gate} =
             GateStore.create(path: tmp, id: "gate_001", run_id: "run_001", status: "waiting")

    assert {:ok, _gate, :inserted} = Store.upsert_gate(store, gate_payload(daemon_x))

    assert {:ok, decided} =
             Store.record_gate_decision(store, "gate_001", %{
               "org_slug" => "false-systems",
               "team_slug" => "platform",
               "status" => "approved",
               "decided_by" => "member:reviewer-y",
               "decided_at" => @later,
               "reason" => "Evidence reviewed"
             })

    assert decided["status"] == "approved"

    assert {:ok, second_heartbeat, _session} =
             Store.heartbeat_daemon_session(store, daemon_y["id"], %{
               "session_id" => daemon_y["id"],
               "status" => "available"
             })

    assert second_heartbeat["decisions"] == []

    assert {:ok, heartbeat, _session} =
             Store.heartbeat_daemon_session(store, daemon_x["id"], %{
               "session_id" => daemon_x["id"],
               "status" => "available"
             })

    assert [%{"id" => "gate_001", "status" => "approved"} = decision] = heartbeat["decisions"]
    PubSub.subscribe("run_001")

    assert {:ok, ["gate_001"]} =
             Join.apply_heartbeat_response(%{"decisions" => [decision]}, path: tmp)

    assert_receive %Sykli.Occurrence{type: "ci.team.gate.decision_received"}, 500

    assert {:ok, local_gate} = GateStore.get("gate_001", path: tmp)
    assert local_gate.status == "approved"
    assert local_gate.decided_by == "member:reviewer-y"
    assert local_gate.reason == "Evidence reviewed"
    assert local_gate.decided_at == @later

    assert {:ok, acked, _session} =
             Store.heartbeat_daemon_session(store, daemon_x["id"], %{
               "session_id" => daemon_x["id"],
               "status" => "available",
               "acknowledged_decision_ids" => ["gate_001"]
             })

    assert acked["decisions"] == []

    assert {:ok, ["gate_001"]} =
             Join.apply_heartbeat_response(%{"decisions" => [decision]}, path: tmp)

    refute_receive %Sykli.Occurrence{type: "ci.team.gate.decision_received"}, 100

    assert {:ok, unchanged} = GateStore.get("gate_001", path: tmp)
    assert unchanged.decided_at == @later

    assert {:ok, events} = Store.audit_log(store)
    actions = Enum.map(events, & &1["action"])
    assert "gate.requested" in actions
    assert "gate.decision_recorded" in actions
  end

  test "executor team gate unblocks after coordinator decision arrives by heartbeat", %{
    store: store,
    daemon_x: daemon_x
  } do
    tmp = tmp_dir()
    install_fake_gate_client(store)
    put_team_token("secret")

    assert {:ok, _session} =
             SessionStore.write(
               %{
                 "coordinator" => "http://coordinator.test",
                 "org" => "false-systems",
                 "team" => "platform",
                 "daemon_id" => "daemon-x",
                 "session_id" => daemon_x["id"],
                 "team_id" => daemon_x["team_id"],
                 "labels" => [],
                 "capabilities" => ["local"]
               },
               path: Path.join(tmp, ".sykli")
             )

    task = %Sykli.Graph.Task{
      name: "approve deploy",
      depends_on: [],
      gate: %Sykli.Graph.Task.Gate{strategy: :env, timeout: 3, env_var: "NEVER_SET"}
    }

    run_id = "run_001"
    gate_id = "run_001-approve-deploy"
    PubSub.subscribe(run_id)

    executor =
      Task.async(fn ->
        Executor.run([task], %{task.name => task},
          target: Sykli.Target.Local,
          workdir: tmp,
          run_id: run_id
        )
      end)

    assert_receive {:publish_waiting, "secret", ^gate_id}, 1_000
    assert {:ok, waiting} = Store.get_gate(store, gate_id)
    assert waiting["status"] == "waiting"

    assert {:ok, _decided} =
             Store.record_gate_decision(store, gate_id, %{
               "org_slug" => "false-systems",
               "team_slug" => "platform",
               "status" => "approved",
               "decided_by" => "member:reviewer-y",
               "decided_at" => @later,
               "reason" => "Evidence reviewed"
             })

    assert {:ok, heartbeat, _session} =
             Store.heartbeat_daemon_session(store, daemon_x["id"], %{
               "session_id" => daemon_x["id"],
               "status" => "available"
             })

    assert [%{"id" => ^gate_id} = decision] = heartbeat["decisions"]

    assert {:ok, [^gate_id]} =
             Join.apply_heartbeat_response(%{"decisions" => [decision]}, path: tmp)

    assert_receive %Sykli.Occurrence{type: "ci.team.gate.decision_received"}, 500

    assert {:ok, {:ok, results}} = Task.yield(executor, 1_000)
    assert [%Executor.TaskResult{name: "approve deploy", status: :passed}] = results

    assert {:ok, local_gate} = GateStore.get(gate_id, path: tmp)
    assert local_gate.status == "approved"
    assert local_gate.decided_by == "member:reviewer-y"
  end

  test "publish validates session routing and claimed team", %{
    store: store,
    daemon_other: daemon_other,
    platform: platform
  } do
    unknown = gate_payload(%{"id" => "missing"})
    assert {:error, :team_gate_unknown_session} = Store.upsert_gate(store, unknown)

    mismatch =
      daemon_other
      |> gate_payload()
      |> Map.put("team_slug", "platform")

    assert {:error, :team_gate_team_mismatch} = Store.upsert_gate(store, mismatch)

    assert {:ok, []} = Store.list_gates(store, %{"team_id" => platform["id"]})
  end

  test "decision must claim the gate's stored team", %{store: store, daemon_x: daemon_x} do
    assert {:ok, _gate, :inserted} = Store.upsert_gate(store, gate_payload(daemon_x))

    assert {:error, :team_gate_team_mismatch} =
             Store.record_gate_decision(store, "gate_001", %{
               "org_slug" => "false-systems",
               "team_slug" => "ops",
               "status" => "approved",
               "decided_by" => "member:reviewer",
               "decided_at" => @later,
               "reason" => "Wrong team"
             })
  end

  test "pending gate decisions move when a daemon session is superseded", %{
    store: store,
    daemon_x: daemon_x
  } do
    assert {:ok, _gate, :inserted} = Store.upsert_gate(store, gate_payload(daemon_x))

    assert {:ok, _decided} =
             Store.record_gate_decision(store, "gate_001", %{
               "org_slug" => "false-systems",
               "team_slug" => "platform",
               "status" => "approved",
               "decided_by" => "member:reviewer-y",
               "decided_at" => @later,
               "reason" => "Evidence reviewed"
             })

    assert {:ok, _response, new_session} = daemon_session(store, "daemon-x", "platform")

    assert {:ok, heartbeat, _session} =
             Store.heartbeat_daemon_session(store, new_session["id"], %{
               "session_id" => new_session["id"],
               "status" => "available"
             })

    assert [%{"id" => "gate_001"}] = heartbeat["decisions"]
  end

  defp daemon_session(store, daemon_id, team) do
    Store.create_daemon_session(store, %{
      "daemon_id" => daemon_id,
      "org" => "false-systems",
      "team" => team,
      "labels" => ["local"],
      "capabilities" => ["local"],
      "version" => "0.6.1"
    })
  end

  defp gate_payload(session) do
    %{
      "org_slug" => "false-systems",
      "team_slug" => "platform",
      "daemon_session_id" => session["id"],
      "id" => "gate_001",
      "run_id" => "run_001",
      "work_item_id" => nil,
      "status" => "waiting",
      "decided_by" => nil,
      "decided_at" => nil,
      "reason" => nil
    }
  end

  defp tmp_dir do
    path = Path.join(System.tmp_dir!(), "sykli-gate-sync-#{System.unique_integer([:positive])}")
    on_exit(fn -> File.rm_rf(path) end)
    path
  end

  defp install_fake_gate_client(store) do
    old_client = Application.get_env(:sykli, :team_gate_client)
    old_store = Application.get_env(:sykli, :test_gate_store)
    old_pid = Application.get_env(:sykli, :test_gate_recorder)

    Application.put_env(:sykli, :team_gate_client, __MODULE__.FakeGateClient)
    Application.put_env(:sykli, :test_gate_store, store)
    Application.put_env(:sykli, :test_gate_recorder, self())

    on_exit(fn ->
      restore_app_env(:team_gate_client, old_client)
      restore_app_env(:test_gate_store, old_store)
      restore_app_env(:test_gate_recorder, old_pid)
    end)
  end

  defp put_team_token(token) do
    old = System.get_env("SYKLI_TEAM_TOKEN")
    System.put_env("SYKLI_TEAM_TOKEN", token)
    on_exit(fn -> restore_env("SYKLI_TEAM_TOKEN", old) end)
  end

  defp restore_app_env(key, nil), do: Application.delete_env(:sykli, key)
  defp restore_app_env(key, value), do: Application.put_env(:sykli, key, value)

  defp restore_env(name, nil), do: System.delete_env(name)
  defp restore_env(name, value), do: System.put_env(name, value)

  defp id_sequence(ids) do
    {:ok, agent} = Agent.start_link(fn -> ids end)

    fn ->
      Agent.get_and_update(agent, fn
        [id | rest] -> {id, rest}
        [] -> flunk("id sequence exhausted")
      end)
    end
  end

  defmodule FakeGateClient do
    def publish_waiting(_session, token, summary) do
      store = Application.fetch_env!(:sykli, :test_gate_store)

      payload =
        summary
        |> Sykli.TeamCoordinator.GateDecisionSummary.encode()
        |> Map.put("org_slug", "false-systems")
        |> Map.put("team_slug", "platform")
        |> Map.put("daemon_session_id", "sess_x")

      send(
        Application.fetch_env!(:sykli, :test_gate_recorder),
        {:publish_waiting, token, payload["id"]}
      )

      case Sykli.TeamCoordinator.Store.upsert_gate(store, payload) do
        {:ok, gate, _mode} -> {:ok, gate}
        {:error, reason} -> {:error, reason}
      end
    end
  end
end
