defmodule Sykli.Outbox do
  @moduledoc "File-backed Team Mode outbox for deferred coordinator sync."

  @base_dir ".sykli/outbox"
  @max_drain 100

  def enqueue(kind, payload, opts \\ []) when is_map(payload) do
    with {:ok, dir} <- kind_dir(kind, opts),
         :ok <- File.mkdir_p(dir),
         {:ok, id} <- payload_id(payload),
         {:ok, json} <-
           Jason.encode(Sykli.Services.SecretMasker.mask_deep(payload, secrets()), pretty: true) do
      atomic_write(Path.join(dir, "#{id}.json"), json)
    end
  end

  def drain(kind, sender_fn, opts \\ []) when is_function(sender_fn, 1) do
    with {:ok, dir} <- kind_dir(kind, opts),
         {:ok, files} <- list_files(dir) do
      do_drain(Enum.take(files, @max_drain), sender_fn, 0)
    end
  end

  def pending_count(kind, opts \\ []) do
    with {:ok, dir} <- kind_dir(kind, opts),
         {:ok, files} <- list_files(dir) do
      {:ok, length(files)}
    end
  end

  defp do_drain([], _sender_fn, count), do: {:ok, count}

  defp do_drain([path | rest], sender_fn, count) do
    with {:ok, body} <- File.read(path),
         {:ok, payload} <- Jason.decode(body),
         :ok <- normalize_send(sender_fn.(payload)),
         :ok <- File.rm(path) do
      do_drain(rest, sender_fn, count + 1)
    else
      {:error, reason} -> {:error, count, reason}
    end
  end

  defp normalize_send(:ok), do: :ok
  defp normalize_send({:ok, _}), do: :ok
  defp normalize_send({:error, reason}), do: {:error, reason}

  defp list_files(dir) do
    case File.ls(dir) do
      {:ok, files} ->
        {:ok,
         files
         |> Enum.filter(&String.ends_with?(&1, ".json"))
         |> Enum.sort()
         |> Enum.map(&Path.join(dir, &1))}

      {:error, :enoent} ->
        {:ok, []}

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp kind_dir(kind, opts) when is_binary(kind) do
    if String.contains?(kind, ["..", "/", "\\", <<0>>]) or kind == "" do
      {:error, :team_outbox_invalid_kind}
    else
      base = Path.expand(Path.join(Keyword.get(opts, :path, "."), @base_dir))
      path = Path.expand(Path.join(base, kind))

      if String.starts_with?(path, base <> "/") do
        {:ok, path}
      else
        {:error, :team_outbox_invalid_kind}
      end
    end
  end

  defp payload_id(%{"run" => %{"id" => id}}) when is_binary(id) and id != "", do: {:ok, id}
  defp payload_id(%{"id" => id}) when is_binary(id) and id != "", do: {:ok, id}
  defp payload_id(_payload), do: {:error, :team_run_invalid_payload}

  defp atomic_write(path, json) do
    tmp_path = "#{path}.tmp-#{System.unique_integer([:positive])}"

    with :ok <- File.write(tmp_path, json),
         :ok <- File.rename(tmp_path, path) do
      :ok
    else
      {:error, reason} ->
        File.rm(tmp_path)
        {:error, {:team_outbox_write_failed, reason}}
    end
  end

  defp secrets do
    System.get_env("SYKLI_TEAM_TOKEN")
    |> List.wrap()
    |> Enum.reject(&is_nil/1)
  end
end
