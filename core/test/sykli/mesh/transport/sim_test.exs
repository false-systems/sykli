defmodule Sykli.Mesh.Transport.SimTest do
  use ExUnit.Case, async: false

  alias Sykli.Mesh.Transport.Sim
  alias Sykli.Mesh.Transport.Sim.PidRef

  setup do
    sim =
      start_supervised!(
        {Sim,
         nodes: [
           %{id: "node-a", profile: :worker, capabilities: [:cpu]},
           %{id: "node-b", profile: :worker, capabilities: [:gpu]}
         ],
         seed: 123}
      )

    %{sim: sim}
  end

  test "start_link with two nodes returns both from list_nodes", %{sim: _sim} do
    assert [
             %{id: "node-a", profile: :worker, status: :up},
             %{id: "node-b", profile: :worker, status: :up}
           ] = Sim.list_nodes()
  end

  test "spawn_remote returns a pid_ref with the correct node_id", %{sim: _sim} do
    assert {:ok, %PidRef{node_id: "node-b", local_id: 0, spawned_at_ms: 0}} =
             Sim.spawn_remote("node-b", {Kernel, :self, []})
  end

  test "spawn then send then advance(0) does not deliver", %{sim: sim} do
    {:ok, pid_ref} = Sim.spawn_remote("node-a", {Kernel, :self, []})
    :ok = Sim.send_msg(pid_ref, :hello)

    assert :ok = Sim.advance(sim, 0)
    assert [] == Sim.inbox(sim, pid_ref)
  end

  test "spawn then send then advance(1) delivers message", %{sim: sim} do
    {:ok, pid_ref} = Sim.spawn_remote("node-a", {Kernel, :self, []})
    :ok = Sim.send_msg(pid_ref, :hello)

    assert :ok = Sim.advance(sim, 1)
    assert [:hello] == Sim.inbox(sim, pid_ref)
  end

  test "named instances can use explicit server helpers" do
    {:ok, sim} =
      Sim.start_link(
        name: nil,
        nodes: [%{id: "node-a", profile: :worker, capabilities: [:cpu]}],
        seed: 42
      )

    try do
      assert [%{id: "node-a", profile: :worker, status: :up}] == Sim.list_nodes(sim)
      assert {:ok, pid_ref} = Sim.spawn_remote(sim, "node-a", {Kernel, :self, []})
      assert :ok = Sim.send_msg(sim, pid_ref, :hello)
      assert 0 == Sim.now_ms(sim)
      assert :ok = Sim.advance(sim, 1)
      assert [:hello] == Sim.inbox(sim, pid_ref)
    after
      GenServer.stop(sim)
    end
  end

  test "multiple messages preserve send order in inbox", %{sim: sim} do
    {:ok, pid_ref} = Sim.spawn_remote("node-a", {Kernel, :self, []})

    :ok = Sim.send_msg(pid_ref, :first)
    :ok = Sim.advance(sim, 1)
    :ok = Sim.send_msg(pid_ref, :second)
    :ok = Sim.advance(sim, 1)
    :ok = Sim.send_msg(pid_ref, :third)
    :ok = Sim.advance(sim, 1)

    assert [:first, :second, :third] == Sim.inbox(sim, pid_ref)
  end

  test "same seed and same operations across fresh sim instances are deterministic" do
    for _ <- 1..100 do
      assert deterministic_run(77) == deterministic_run(77)
    end
  end

  test "now_ms is monotonically non-decreasing across operations", %{sim: sim} do
    {:ok, pid_ref} = Sim.spawn_remote("node-a", {Kernel, :self, []})

    timestamps = [
      Sim.now_ms(),
      tap(Sim.send_msg(pid_ref, :hello), fn :ok -> :ok end) && Sim.now_ms(),
      tap(Sim.advance(sim, 0), fn :ok -> :ok end) && Sim.now_ms(),
      tap(Sim.advance(sim, 1), fn :ok -> :ok end) && Sim.now_ms(),
      tap(Sim.send_msg(pid_ref, :again), fn :ok -> :ok end) && Sim.now_ms(),
      tap(Sim.advance(sim, 2), fn :ok -> :ok end) && Sim.now_ms()
    ]

    assert timestamps == Enum.sort(timestamps)
  end

  test "advance(0) is a no-op", %{sim: sim} do
    {:ok, pid_ref} = Sim.spawn_remote("node-a", {Kernel, :self, []})
    :ok = Sim.send_msg(pid_ref, :hello)
    before_ms = Sim.now_ms()

    assert :ok = Sim.advance(sim, 0)
    assert before_ms == Sim.now_ms()
    assert [] == Sim.inbox(sim, pid_ref)
  end

  test "unknown destinations are dropped without crashing", %{sim: sim} do
    unknown = %PidRef{node_id: "node-a", local_id: 99, spawned_at_ms: 0}

    assert :ok = Sim.send_msg(unknown, :hello)
    assert :ok = Sim.advance(sim, 1)
    assert [] == Sim.inbox(sim, unknown)
  end

  defp deterministic_run(seed) do
    {:ok, sim} =
      Sim.start_link(
        name: nil,
        nodes: [%{id: "node-a", profile: :worker, capabilities: [:cpu]}],
        seed: seed
      )

    try do
      {:ok, pid_ref} = GenServer.call(sim, {:spawn_remote, "node-a", {Kernel, :self, []}})
      :ok = GenServer.call(sim, {:send_msg, pid_ref, :alpha})
      :ok = GenServer.call(sim, {:advance, 1})
      :ok = GenServer.call(sim, {:send_msg, pid_ref, :beta})
      :ok = GenServer.call(sim, {:advance, 1})
      GenServer.call(sim, {:inbox, pid_ref})
    after
      GenServer.stop(sim)
    end
  end
end
