defmodule Sykli.TeamCoordinator.GateOutboxTest do
  use ExUnit.Case, async: false

  alias Sykli.Daemon.SessionStore
  alias Sykli.Executor
  alias Sykli.Occurrence.PubSub
  alias Sykli.Outbox
  alias Sykli.TeamCoordinator.Store

  @moduletag :tmp_dir

  @payload %{
    "org_slug" => "false-systems",
    "team_slug" => "platform",
    "daemon_session_id" => "sess_001",
    "id" => "gate_001",
    "run_id" => "run_001",
    "work_item_id" => nil,
    "status" => "waiting",
    "decided_by" => nil,
    "decided_at" => nil,
    "reason" => nil
  }

  test "gate outbox replays a deferred publish after coordinator outage", %{tmp_dir: tmp_dir} do
    payload = Map.put(@payload, "daemon_session_id", "sess_001")

    assert :ok = Outbox.enqueue("gates", payload, path: tmp_dir)
    assert File.exists?(Path.join([tmp_dir, ".sykli", "outbox", "gates", "gate_001.json"]))

    {:ok, store} =
      Store.start_link(
        now: fn -> "2026-05-09T10:00:00Z" end,
        id:
          id_sequence(~w(org_001 audit_001 team_001 audit_002 sess_001 audit_003 gate_audit_001))
      )

    {:ok, org} = Store.create_org(store, %{"slug" => "false-systems", "name" => "False Systems"})

    {:ok, _team} =
      Store.create_team(store, %{
        "org_id" => org["id"],
        "slug" => "platform",
        "name" => "Platform"
      })

    {:ok, _response, _session} =
      Store.create_daemon_session(store, %{
        "daemon_id" => "daemon-x",
        "org" => "false-systems",
        "team" => "platform",
        "labels" => [],
        "capabilities" => ["local"],
        "version" => "0.6.1"
      })

    assert {:ok, 1} =
             Outbox.drain("gates", fn payload -> Store.upsert_gate(store, payload) end,
               path: tmp_dir
             )

    assert {:ok, [%{"id" => "gate_001"}]} = Store.list_gates(store, %{})

    assert {:ok, events} = Store.audit_log(store)
    assert Enum.any?(events, &(&1["action"] == "gate.requested"))
  end

  test "deferred executor gate publish emits requested and sync_deferred occurrences", %{
    tmp_dir: tmp_dir
  } do
    old_client = Application.get_env(:sykli, :team_gate_client)
    old_token = System.get_env("SYKLI_TEAM_TOKEN")

    Application.put_env(:sykli, :team_gate_client, __MODULE__.FailingGateClient)
    System.put_env("SYKLI_TEAM_TOKEN", "secret")

    on_exit(fn ->
      restore_app_env(:team_gate_client, old_client)
      restore_env("SYKLI_TEAM_TOKEN", old_token)
    end)

    assert {:ok, _session} =
             SessionStore.write(
               %{
                 "coordinator" => "http://coordinator.test",
                 "org" => "false-systems",
                 "team" => "platform",
                 "daemon_id" => "daemon-x",
                 "session_id" => "sess_001",
                 "team_id" => "team_001",
                 "labels" => [],
                 "capabilities" => ["local"]
               },
               path: Path.join(tmp_dir, ".sykli")
             )

    task = %Sykli.Graph.Task{
      name: "approve",
      depends_on: [],
      gate: %Sykli.Graph.Task.Gate{strategy: :env, timeout: 1, env_var: "NEVER_SET"}
    }

    PubSub.subscribe("run_001")

    assert {:error, [%Executor.TaskResult{name: "approve", status: :failed}]} =
             Executor.run([task], %{"approve" => task},
               target: Sykli.Target.Local,
               workdir: tmp_dir,
               run_id: "run_001"
             )

    assert_receive %Sykli.Occurrence{type: "ci.team.gate.requested"}, 500
    assert_receive %Sykli.Occurrence{type: "ci.team.gate.sync_deferred"}, 500
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

  defp restore_app_env(key, nil), do: Application.delete_env(:sykli, key)
  defp restore_app_env(key, value), do: Application.put_env(:sykli, key, value)

  defp restore_env(name, nil), do: System.delete_env(name)
  defp restore_env(name, value), do: System.put_env(name, value)

  defmodule FailingGateClient do
    def publish_waiting(_session, _token, _summary), do: {:error, :unreachable}
  end
end
