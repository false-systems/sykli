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
