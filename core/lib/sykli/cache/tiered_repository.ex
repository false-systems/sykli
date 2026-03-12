defmodule Sykli.Cache.TieredRepository do
  @moduledoc """
  Write-through tiered cache: local L1 + S3 L2.

  Reads check L1 first, then L2. Writes go to both.
  This gives fast local hits while sharing cache across CI workers.
  """

  @behaviour Sykli.Cache.Repository

  alias Sykli.Cache.Entry
  alias Sykli.Cache.FileRepository, as: L1
  alias Sykli.Cache.S3Repository, as: L2

  @impl true
  def init do
    L1.init()
    # S3 init is a no-op
    L2.init()
  end

  @impl true
  def get(key) do
    case L1.get(key) do
      {:ok, entry} ->
        {:ok, entry}

      {:error, _} ->
        case L2.get(key) do
          {:ok, entry} ->
            # Promote to L1
            L1.put(key, entry)
            {:ok, entry}

          error ->
            error
        end
    end
  end

  @impl true
  def put(key, %Entry{} = entry) do
    L1.put(key, entry)
    L2.put(key, entry)
  end

  @impl true
  def delete(key) do
    L1.delete(key)
    L2.delete(key)
    :ok
  end

  @impl true
  def exists?(key) do
    L1.exists?(key) or L2.exists?(key)
  end

  @impl true
  def list_keys do
    local_keys = L1.list_keys() |> MapSet.new()
    remote_keys = L2.list_keys() |> MapSet.new()
    MapSet.union(local_keys, remote_keys) |> MapSet.to_list()
  end

  @impl true
  def store_blob(content) do
    case L1.store_blob(content) do
      {:ok, hash} ->
        # Also push to L2
        L2.store_blob(content)
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
        case L2.get_blob(hash) do
          {:ok, content} ->
            # Promote to L1
            L1.store_blob(content)
            {:ok, content}

          error ->
            error
        end
    end
  end

  @impl true
  def blob_exists?(hash) do
    L1.blob_exists?(hash) or L2.blob_exists?(hash)
  end

  @impl true
  def stats, do: L1.stats()

  @impl true
  def clean do
    L1.clean()
    L2.clean()
    :ok
  end

  @impl true
  def clean_older_than(seconds) do
    L1.clean_older_than(seconds)
  end
end
