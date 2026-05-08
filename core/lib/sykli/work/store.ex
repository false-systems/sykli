defmodule Sykli.Work.Store do
  @moduledoc """
  File-backed local work item store.

  The store persists work items under `.sykli/work/items/<id>.json` in the
  requested project path. It is intentionally local-only; coordinator sync is a
  later Team Mode phase.

  Writes are atomic per file, but this store does not provide cross-process
  locking. Read-modify-write operations, including claims, enforce model rules
  for the loaded item and remain last-writer-wins under concurrent writers until
  the coordinator store adds transactional claims.
  """

  alias Sykli.WorkItem

  @items_dir ".sykli/work/items"

  @doc "Returns the work item storage directory for a base path."
  def items_dir(opts \\ []) do
    opts
    |> base_path()
    |> Path.join(@items_dir)
  end

  @doc "Creates and persists a new work item."
  def create(title, opts \\ []) do
    with {:ok, item} <- WorkItem.new(title, opts),
         :ok <- save(item, opts) do
      {:ok, item}
    end
  end

  @doc "Persists an existing work item."
  def save(%WorkItem{} = item, opts \\ []) do
    with :ok <- WorkItem.validate_id(item.id),
         {:ok, path} <- item_path(item.id, opts),
         :ok <- File.mkdir_p(Path.dirname(path)),
         {:ok, json} <- encode(item),
         :ok <- atomic_write(path, json) do
      :ok
    end
  end

  @doc "Loads a work item by id."
  def get(id, opts \\ []) do
    with {:ok, path} <- item_path(id, opts) do
      case File.read(path) do
        {:ok, json} -> decode(json, path)
        {:error, :enoent} -> {:error, {:work_item_not_found, id}}
        {:error, reason} -> {:error, {:work_item_read_failed, id, reason}}
      end
    end
  end

  @doc "Lists all persisted work items in deterministic id order."
  def list(opts \\ []) do
    dir = items_dir(opts)

    case File.ls(dir) do
      {:ok, files} ->
        files
        |> Enum.filter(&String.ends_with?(&1, ".json"))
        |> Enum.sort()
        |> Enum.reduce_while({:ok, []}, fn file, {:ok, items} ->
          id = String.trim_trailing(file, ".json")

          case get(id, opts) do
            {:ok, item} -> {:cont, {:ok, [item | items]}}
            {:error, reason} -> {:halt, {:error, reason}}
          end
        end)
        |> case do
          {:ok, items} -> {:ok, Enum.reverse(items)}
          error -> error
        end

      {:error, :enoent} ->
        {:ok, []}

      {:error, reason} ->
        {:error, {:work_item_list_failed, reason}}
    end
  end

  @doc "Updates a persisted work item's status."
  def update_status(id, status, opts \\ []) do
    with {:ok, item} <- get(id, opts),
         {:ok, updated} <- WorkItem.update_status(item, status, opts),
         :ok <- save(updated, opts) do
      {:ok, updated}
    end
  end

  @doc "Claims a persisted work item."
  def claim(id, assignment_type, assignment_id, opts \\ []) do
    with {:ok, item} <- get(id, opts),
         {:ok, updated} <- WorkItem.claim(item, assignment_type, assignment_id, opts),
         :ok <- save(updated, opts) do
      {:ok, updated}
    end
  end

  @doc "Appends a note to a persisted work item."
  def append_note(id, body, opts \\ []) do
    with {:ok, item} <- get(id, opts),
         {:ok, updated} <- WorkItem.append_note(item, body, opts),
         :ok <- save(updated, opts) do
      {:ok, List.last(updated.notes), updated}
    end
  end

  defp item_path(id, opts) do
    with :ok <- WorkItem.validate_id(id) do
      dir = items_dir(opts)
      path = Path.expand(Path.join(dir, "#{id}.json"))
      expanded_dir = Path.expand(dir)

      if path_within?(path, expanded_dir) do
        {:ok, path}
      else
        {:error, {:work_item_path_escape, id}}
      end
    end
  end

  defp encode(%WorkItem{} = item) do
    case Jason.encode(WorkItem.to_map(item), pretty: true) do
      {:ok, json} -> {:ok, json}
      {:error, error} -> {:error, {:work_item_encode_failed, error}}
    end
  end

  defp decode(json, path) do
    with {:ok, data} <- Jason.decode(json),
         {:ok, item} <- WorkItem.from_map(data) do
      {:ok, item}
    else
      {:error, %Jason.DecodeError{} = error} -> {:error, {:malformed_work_item_json, path, error}}
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
        {:error, {:work_item_write_failed, path, reason}}
    end
  end

  defp base_path(opts), do: Keyword.get(opts, :path, ".")

  defp path_within?(path, base) do
    expanded_path = Path.expand(path)
    expanded_base = Path.expand(base)

    expanded_path == expanded_base or String.starts_with?(expanded_path, expanded_base <> "/")
  end
end
