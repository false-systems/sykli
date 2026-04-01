defmodule Sykli.Cache.TieredRepository do
  @moduledoc """
  Write-through tiered cache: local L1 + S3 L2.

  Reads check L1 first, then L2. Writes go to L1 synchronously
  and L2 asynchronously (never blocks the executor).

  Includes a circuit breaker for L2: after `@failure_threshold`
  consecutive S3 failures, L2 writes are skipped for a cooldown
  window. This prevents cascading timeouts when S3 is unreachable.
  """

  @behaviour Sykli.Cache.Repository

  require Logger

  alias Sykli.Cache.Entry
  alias Sykli.Cache.FileRepository, as: L1
  alias Sykli.Cache.S3Repository, as: L2

  # Circuit breaker settings
  @failure_threshold 5
  @cooldown_ms 60_000

  @impl true
  def init do
    L1.init()
    L2.init()
    # Initialize circuit breaker state in persistent_term
    :persistent_term.put(:sykli_s3_circuit, %{failures: 0, open_until: 0})
  end

  @impl true
  def get(key) do
    case L1.get(key) do
      {:ok, entry} ->
        {:ok, entry}

      {:error, _} ->
        if circuit_closed?() do
          case L2.get(key) do
            {:ok, entry} ->
              record_success()
              # Promote to L1
              L1.put(key, entry)
              {:ok, entry}

            error ->
              record_failure()
              error
          end
        else
          {:error, :s3_circuit_open}
        end
    end
  end

  @impl true
  def put(key, %Entry{} = entry) do
    L1.put(key, entry)
    async_l2(fn -> L2.put(key, entry) end)
  end

  @impl true
  def delete(key) do
    L1.delete(key)
    async_l2(fn -> L2.delete(key) end)
    :ok
  end

  @impl true
  def exists?(key) do
    L1.exists?(key) or (circuit_closed?() and L2.exists?(key))
  end

  @impl true
  def list_keys do
    local_keys = L1.list_keys() |> MapSet.new()

    remote_keys =
      if circuit_closed?() do
        L2.list_keys() |> MapSet.new()
      else
        MapSet.new()
      end

    MapSet.union(local_keys, remote_keys) |> MapSet.to_list()
  end

  @impl true
  def store_blob(content) do
    case L1.store_blob(content) do
      {:ok, hash} ->
        async_l2(fn -> L2.store_blob(content) end)
        {:ok, hash}

      error ->
        error
    end
  end

  @impl true
  def get_blob(hash) do
    case L1.get_blob(hash) do
      {:ok, content} ->
        {:ok, content}

      {:error, _} ->
        if circuit_closed?() do
          case L2.get_blob(hash) do
            {:ok, content} ->
              record_success()
              L1.store_blob(content)
              {:ok, content}

            error ->
              record_failure()
              error
          end
        else
          {:error, :s3_circuit_open}
        end
    end
  end

  @impl true
  def blob_exists?(hash) do
    L1.blob_exists?(hash) or (circuit_closed?() and L2.blob_exists?(hash))
  end

  @impl true
  def stats, do: L1.stats()

  @impl true
  def clean do
    L1.clean()
    async_l2(fn -> L2.clean() end)
    :ok
  end

  @impl true
  def clean_older_than(seconds) do
    L1.clean_older_than(seconds)
  end

  # ----- ASYNC L2 WRITES -----

  defp async_l2(fun) do
    if circuit_closed?() do
      Task.Supervisor.async_nolink(Sykli.TaskSupervisor, fn ->
        try do
          fun.()
          record_success()
        rescue
          e ->
            record_failure()
            Logger.warning("[TieredCache] S3 write failed: #{inspect(e)}")
        end
      end)
    end

    :ok
  end

  # ----- CIRCUIT BREAKER -----

  defp circuit_closed? do
    state = get_circuit_state()

    cond do
      state.failures < @failure_threshold -> true
      System.monotonic_time(:millisecond) >= state.open_until -> true
      true -> false
    end
  end

  defp record_success do
    state = get_circuit_state()

    if state.failures > 0 do
      if state.failures >= @failure_threshold do
        Logger.info("[TieredCache] S3 circuit breaker closed (recovered)")
      end

      :persistent_term.put(:sykli_s3_circuit, %{failures: 0, open_until: 0})
    end
  end

  defp record_failure do
    state = get_circuit_state()
    new_failures = state.failures + 1

    if new_failures >= @failure_threshold and state.failures < @failure_threshold do
      open_until = System.monotonic_time(:millisecond) + @cooldown_ms

      Logger.warning(
        "[TieredCache] S3 circuit breaker OPEN after #{new_failures} consecutive failures " <>
          "(cooldown: #{@cooldown_ms}ms)"
      )

      :persistent_term.put(:sykli_s3_circuit, %{failures: new_failures, open_until: open_until})
    else
      :persistent_term.put(:sykli_s3_circuit, %{state | failures: new_failures})
    end
  end

  defp get_circuit_state do
    :persistent_term.get(:sykli_s3_circuit)
  rescue
    ArgumentError -> %{failures: 0, open_until: 0}
  end
end
