defmodule Sykli.Mesh.Transport.Sim do
  @moduledoc """
  Deterministic in-process simulator transport backend.
  """

  use GenServer

  alias Sykli.Mesh.Transport.Sim.EventQueue
  alias Sykli.Mesh.Transport.Sim.Network
  alias Sykli.Mesh.Transport.Sim.PidRef
  alias Sykli.Mesh.Transport.Sim.Rng
  alias Sykli.Mesh.Transport.Sim.SimNode
  alias Sykli.Mesh.Transport.Sim.State

  @behaviour Sykli.Mesh.Transport

  @type process_info :: %{mfa: {module(), atom(), [term()]}, inbox: [term()]}
  @type trace_state :: %{
          seed: integer(),
          next_local_id: non_neg_integer(),
          monitors: %{reference() => term()}
        }

  @spec start_link(keyword()) :: GenServer.on_start()
  def start_link(opts) do
    server_opts =
      case Keyword.get(opts, :name, __MODULE__) do
        nil -> []
        name -> [name: name]
      end

    GenServer.start_link(__MODULE__, opts, server_opts)
  end

  @spec advance(pid(), non_neg_integer()) :: :ok
  def advance(sim, ms) do
    GenServer.call(sim, {:advance, ms})
  end

  @spec inbox(pid(), term()) :: [term()]
  def inbox(sim, pid_ref) do
    GenServer.call(sim, {:inbox, pid_ref})
  end

  @spec reset(pid()) :: :ok
  def reset(sim) do
    GenServer.call(sim, :reset)
  end

  @impl true
  def list_nodes do
    GenServer.call(__MODULE__, :list_nodes)
  end

  @impl true
  def spawn_remote(node_id, mfa) do
    GenServer.call(__MODULE__, {:spawn_remote, node_id, mfa})
  end

  @impl true
  def send_msg(destination, message) do
    GenServer.call(__MODULE__, {:send_msg, destination, message})
  end

  @impl true
  def monitor(target) do
    GenServer.call(__MODULE__, {:monitor, target})
  end

  @impl true
  def demonitor(reference) do
    GenServer.call(__MODULE__, {:demonitor, reference})
  end

  @impl true
  def now_ms do
    GenServer.call(__MODULE__, :now_ms)
  end

  @impl true
  def emit(_event) do
    raise RuntimeError, ":not_implemented"
  end

  @impl true
  def subscribe(_opts) do
    raise RuntimeError, ":not_implemented"
  end

  @impl true
  def init(opts) do
    seed = Keyword.get(opts, :seed, 0)
    node_specs = Keyword.get(opts, :nodes, [])

    {:ok,
     %State{
       nodes: build_nodes(node_specs),
       clock: 0,
       event_queue: EventQueue.new(),
       seq: 0,
       rng: Rng.new(seed),
       network: %Network{},
       trace: %{seed: seed, next_local_id: 0, monitors: %{}},
       emit_subscribers: []
     }}
  end

  @impl true
  def handle_call(:list_nodes, _from, state) do
    nodes =
      state.nodes
      |> Map.values()
      |> Enum.map(fn %SimNode{id: id, profile: profile, status: status} ->
        %{id: id, profile: profile, status: status}
      end)

    {:reply, nodes, state}
  end

  def handle_call({:spawn_remote, node_id, mfa}, _from, state) do
    case Map.fetch(state.nodes, node_id) do
      {:ok, node} ->
        pid_ref = %PidRef{
          node_id: node_id,
          local_id: state.trace.next_local_id,
          spawned_at_ms: state.clock
        }

        process_info = %{mfa: mfa, inbox: []}

        next_state =
          put_in(state.nodes[node_id], %{
            node
            | processes: Map.put(node.processes, pid_ref, process_info)
          })
          |> put_in(
            [Access.key(:trace), Access.key(:next_local_id)],
            state.trace.next_local_id + 1
          )

        {:reply, {:ok, pid_ref}, next_state}

      :error ->
        {:reply, {:error, :unknown_node}, state}
    end
  end

  def handle_call({:send_msg, %PidRef{} = destination, message}, _from, state) do
    next_seq = state.seq + 1
    deliver_at = state.clock + 1
    event = {:deliver, destination, message}

    next_state = %{
      state
      | seq: next_seq,
        event_queue: EventQueue.insert(state.event_queue, deliver_at, next_seq, event)
    }

    {:reply, :ok, next_state}
  end

  def handle_call({:monitor, target}, _from, state) do
    reference = make_ref()
    next_state = put_in(state.trace.monitors[reference], target)
    {:reply, reference, next_state}
  end

  def handle_call({:demonitor, reference}, _from, state) do
    next_state = update_in(state.trace.monitors, &Map.delete(&1, reference))
    {:reply, :ok, next_state}
  end

  def handle_call(:now_ms, _from, state) do
    {:reply, state.clock, state}
  end

  def handle_call({:advance, 0}, _from, state) do
    {:reply, :ok, state}
  end

  def handle_call({:advance, ms}, _from, state) do
    target_ms = state.clock + ms
    next_state = advance_to(state, target_ms)
    {:reply, :ok, next_state}
  end

  def handle_call({:inbox, %PidRef{} = pid_ref}, _from, state) do
    inbox =
      state.nodes
      |> Map.get(pid_ref.node_id, %SimNode{processes: %{}})
      |> Map.get(:processes, %{})
      |> Map.get(pid_ref, %{inbox: []})
      |> Map.get(:inbox, [])

    {:reply, inbox, state}
  end

  def handle_call(:reset, _from, state) do
    {:reply, :ok, reset_state(state)}
  end

  defp build_nodes(node_specs) do
    node_specs
    |> Enum.map(fn node ->
      sim_node = %SimNode{
        id: node.id,
        profile: node.profile,
        status: Map.get(node, :status, :up),
        capabilities: Map.get(node, :capabilities, []),
        processes: %{},
        inbox: []
      }

      {sim_node.id, sim_node}
    end)
    |> Map.new()
  end

  defp advance_to(state, target_ms) do
    {events, queue} = EventQueue.drain_until(state.event_queue, target_ms)

    state_after_events =
      Enum.reduce(events, %{state | event_queue: queue}, fn {at_ms, _seq, event}, acc ->
        acc
        |> Map.put(:clock, at_ms)
        |> apply_event(event)
      end)

    %{state_after_events | clock: target_ms}
  end

  defp apply_event(state, {:deliver, %PidRef{} = destination, message}) do
    update_in(state.nodes[destination.node_id].processes[destination].inbox, &(&1 ++ [message]))
  end

  defp reset_state(state) do
    %{
      state
      | nodes: reset_nodes(state.nodes),
        clock: 0,
        event_queue: EventQueue.new(),
        seq: 0,
        rng: Rng.new(state.trace.seed),
        trace: %{state.trace | next_local_id: 0, monitors: %{}},
        emit_subscribers: []
    }
  end

  defp reset_nodes(nodes) do
    Map.new(nodes, fn {id, node} ->
      {id, %{node | processes: %{}, inbox: []}}
    end)
  end
end
