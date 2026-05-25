defmodule Sykli.Outbox do
  @moduledoc "File-backed Team Mode outbox for deferred coordinator sync."

  @base_dir ".sykli/outbox"
  @max_drain 100

  def enqueue(kind, payload, opts \\ []) when is_map(payload) do
    secrets = Sykli.Services.SecretPatterns.all_values(Keyword.get(opts, :secrets, []))

    with {:ok, dir} <- kind_dir(kind, opts),
         :ok <- File.mkdir_p(dir),
         {:ok, id} <- payload_id(payload),
         {:ok, json} <-
           Jason.encode(Sykli.Services.SecretMasker.mask_deep(payload, secrets)) do
      atomic_write(Path.join(dir, "#{id}.json"), json)
    end
  end

  def delete(kind, payload, opts \\ []) when is_map(payload) do
    with {:ok, dir} <- kind_dir(kind, opts),
         {:ok, id} <- payload_id(payload) do
      case File.rm(Path.join(dir, "#{id}.json")) do
        :ok -> :ok
        {:error, :enoent} -> :ok
        {:error, reason} -> {:error, reason}
      end
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
    case read_payload(path) do
      {:ok, payload} ->
        case normalize_send(sender_fn.(payload)) do
          :ok ->
            :ok = File.rm(path)
            do_drain(rest, sender_fn, count + 1)

          {:error, reason} ->
            if permanent_failure?(reason) do
              :ok = File.rm(path)
              do_drain(rest, sender_fn, count)
            else
              {:error, count, reason}
            end
        end

      {:error, reason} ->
        if permanent_failure?(reason) do
          :ok = File.rm(path)
          do_drain(rest, sender_fn, count)
        else
          {:error, count, reason}
        end
    end
  end

  defp read_payload(path) do
    with {:ok, body} <- File.read(path),
         {:ok, payload} <- Jason.decode(body) do
      {:ok, payload}
    else
      {:error, %Jason.DecodeError{} = error} -> {:error, {:team_run_invalid_payload, error}}
      {:error, reason} -> {:error, reason}
    end
  end

  defp normalize_send(:ok), do: :ok
  defp normalize_send({:ok, _}), do: :ok
  defp normalize_send({:ok, _, _}), do: :ok
  defp normalize_send({:error, reason}), do: {:error, reason}

  defp permanent_failure?(:team_run_invalid_payload), do: true
  defp permanent_failure?(:team_run_body_too_large), do: true
  defp permanent_failure?(:team_gate_invalid_payload), do: true
  defp permanent_failure?(:team_gate_body_too_large), do: true
  defp permanent_failure?({:team_run_invalid_payload, _reason}), do: true
  defp permanent_failure?({:team_gate_invalid_payload, _reason}), do: true

  defp permanent_failure?({:team_coordinator_error, %{"code" => code}})
       when code in [
              "team.run.invalid_payload",
              "team.run.body_too_large",
              "gate.invalid_payload",
              "gate.body_too_large"
            ],
       do: true

  defp permanent_failure?(_reason), do: false

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
end
