defmodule Sykli.Gate.Store do
  @moduledoc """
  File-backed local gate decision store.

  The store persists gate decisions under `.sykli/gates/<id>.json` in the
  requested project path. It is intentionally local-only; coordinator sync and
  runtime gate lifecycle integration are later Team Mode phases.
  """

  alias Sykli.GateDecision

  @gates_dir ".sykli/gates"

  @doc "Returns the gate storage directory for a base path."
  def gates_dir(opts \\ []) do
    opts
    |> base_path()
    |> Path.join(@gates_dir)
  end

  @doc "Creates and persists a new gate request."
  def create(opts \\ []) do
    with {:ok, gate} <- GateDecision.new(opts),
         :ok <- save(gate, opts) do
      {:ok, gate}
    end
  end

  @doc "Persists an existing gate decision."
  def save(%GateDecision{} = gate, opts \\ []) do
    with :ok <- GateDecision.validate_id(gate.id),
         {:ok, path} <- gate_path(gate.id, opts),
         :ok <- File.mkdir_p(Path.dirname(path)),
         {:ok, json} <- encode(gate),
         :ok <- atomic_write(path, json) do
      :ok
    end
  end

  @doc "Loads a gate decision by id."
  def get(id, opts \\ []) do
    with {:ok, path} <- gate_path(id, opts) do
      case File.read(path) do
        {:ok, json} -> decode(json, path)
        {:error, :enoent} -> {:error, {:gate_not_found, id}}
        {:error, reason} -> {:error, {:gate_read_failed, id, reason}}
      end
    end
  end

  @doc "Lists all persisted gate decisions in deterministic id order."
  def list(opts \\ []) do
    dir = gates_dir(opts)

    case File.ls(dir) do
      {:ok, files} ->
        files
        |> Enum.filter(&String.ends_with?(&1, ".json"))
        |> Enum.sort()
        |> Enum.reduce_while({:ok, []}, fn file, {:ok, gates} ->
          id = String.trim_trailing(file, ".json")

          case get(id, opts) do
            {:ok, gate} -> {:cont, {:ok, [gate | gates]}}
            {:error, reason} -> {:halt, {:error, reason}}
          end
        end)
        |> case do
          {:ok, gates} -> {:ok, Enum.reverse(gates)}
          error -> error
        end

      {:error, :enoent} ->
        {:ok, []}

      {:error, reason} ->
        {:error, {:gate_list_failed, reason}}
    end
  end

  @doc "Lists persisted gate decisions matching a status."
  def list_by_status(status, opts \\ []) do
    with :ok <- GateDecision.validate_status(status),
         {:ok, gates} <- list(opts) do
      {:ok, Enum.filter(gates, &(&1.status == status))}
    end
  end

  @doc "Approves a persisted gate decision."
  def approve(id, reason, opts \\ []) do
    with {:ok, gate} <- get(id, opts),
         {:ok, updated} <- GateDecision.approve(gate, reason, opts),
         :ok <- save(updated, opts) do
      {:ok, updated}
    end
  end

  @doc "Rejects a persisted gate decision."
  def reject(id, reason, opts \\ []) do
    with {:ok, gate} <- get(id, opts),
         {:ok, updated} <- GateDecision.reject(gate, reason, opts),
         :ok <- save(updated, opts) do
      {:ok, updated}
    end
  end

  @doc "Applies a coordinator-delivered gate decision to local state."
  def apply_remote_decision(payload, opts \\ [])

  def apply_remote_decision(payload, opts) when is_map(payload) do
    with {:ok, id} <- required_string(payload, "id"),
         {:ok, gate} <- get(id, opts),
         {:ok, result} <- remote_transition(gate, payload, opts) do
      case result do
        {:changed, updated} ->
          :ok = save(updated, opts)
          emit_decision_received(updated, payload)
          broadcast_gate_decision(updated)
          {:ok, updated, :changed}

        {:unchanged, gate} ->
          {:ok, gate, :unchanged}
      end
    end
  end

  def apply_remote_decision(_payload, _opts), do: {:error, {:invalid_gate_decision, :not_object}}

  defp gate_path(id, opts) do
    with :ok <- GateDecision.validate_id(id) do
      dir = gates_dir(opts)
      path = Path.expand(Path.join(dir, "#{id}.json"))
      expanded_dir = Path.expand(dir)

      if path_within?(path, expanded_dir) do
        {:ok, path}
      else
        {:error, {:gate_path_escape, id}}
      end
    end
  end

  defp remote_transition(%GateDecision{} = gate, payload, opts) do
    status = payload["status"]
    reason = payload["reason"]
    decided_by = payload["decided_by"]

    cond do
      gate.status == status and gate.reason == reason and gate.decided_by == decided_by ->
        {:ok, {:unchanged, gate}}

      gate.status not in ["waiting", "blocked"] ->
        {:ok, {:unchanged, gate}}

      status == "approved" ->
        gate
        |> GateDecision.approve(reason, remote_opts(payload, opts, decided_by))
        |> changed()

      status == "rejected" ->
        gate
        |> GateDecision.reject(reason, remote_opts(payload, opts, decided_by))
        |> changed()

      true ->
        {:error, {:invalid_gate_status, status}}
    end
  end

  defp changed({:ok, gate}), do: {:ok, {:changed, gate}}
  defp changed({:error, reason}), do: {:error, reason}

  defp remote_opts(payload, opts, decided_by) do
    opts
    |> Keyword.take([:now])
    |> Keyword.put(:decided_by, decided_by)
    |> maybe_put_now(payload["decided_at"])
  end

  defp maybe_put_now(opts, nil), do: opts
  defp maybe_put_now(opts, ""), do: opts
  defp maybe_put_now(opts, decided_at), do: Keyword.put(opts, :now, decided_at)

  defp emit_decision_received(%GateDecision{} = gate, payload) do
    run_id = gate.run_id || payload["run_id"] || "unknown"

    Sykli.Occurrence.PubSub.team_gate_decision_received(run_id, %{
      "id" => gate.id,
      "run_id" => gate.run_id,
      "work_item_id" => gate.work_item_id,
      "status" => gate.status,
      "decided_by" => gate.decided_by,
      "decided_at" => gate.decided_at,
      "reason" => gate.reason
    })
  end

  defp broadcast_gate_decision(%GateDecision{} = gate) do
    Phoenix.PubSub.broadcast(
      Sykli.PubSub,
      "gate:" <> gate.id,
      {:gate_decided, gate.status, gate.decided_by || "team"}
    )
  end

  defp required_string(map, field) do
    case Map.get(map, field) do
      value when is_binary(value) ->
        value = String.trim(value)
        if value == "", do: {:error, {:invalid_gate_field, field, :empty}}, else: {:ok, value}

      _ ->
        {:error, {:invalid_gate_field, field, nil}}
    end
  end

  defp encode(%GateDecision{} = gate) do
    case Jason.encode(GateDecision.to_map(gate), pretty: true) do
      {:ok, json} -> {:ok, json}
      {:error, error} -> {:error, {:gate_encode_failed, error}}
    end
  end

  defp decode(json, path) do
    with {:ok, data} <- Jason.decode(json),
         {:ok, gate} <- GateDecision.from_map(data) do
      {:ok, gate}
    else
      {:error, %Jason.DecodeError{} = error} -> {:error, {:malformed_gate_json, path, error}}
      {:error, reason} -> {:error, reason}
    end
  end

  defp atomic_write(path, json) do
    tmp_path = "#{path}.tmp-#{System.unique_integer([:positive])}"

    with :ok <- File.write(tmp_path, json),
         :ok <- File.rename(tmp_path, path) do
      :ok
    else
      {:error, reason} ->
        File.rm(tmp_path)
        {:error, {:gate_write_failed, path, reason}}
    end
  end

  defp base_path(opts), do: Keyword.get(opts, :path, ".")

  defp path_within?(path, base) do
    expanded_path = Path.expand(path)
    expanded_base = Path.expand(base)

    expanded_path == expanded_base or String.starts_with?(expanded_path, expanded_base <> "/")
  end
end
