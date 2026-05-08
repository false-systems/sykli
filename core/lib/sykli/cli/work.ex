defmodule Sykli.CLI.Work do
  @moduledoc """
  Local work item CLI commands.

  This module is intentionally local-only. Team/coordinator routing is added in
  later Team Mode phases.
  """

  alias Sykli.CLI.JsonResponse
  alias Sykli.Error
  alias Sykli.Error.Formatter
  alias Sykli.Work.Store
  alias Sykli.WorkItem

  @default_actor_type "member"
  @default_actor_id "local"

  @doc """
  Runs a `sykli work ...` command and returns the intended process exit code.
  """
  def run(args, runtime_opts \\ []) do
    case parse(args) do
      {:ok, command, opts} ->
        execute(command, opts, runtime_opts)

      {:error, reason, opts} ->
        output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute({:create, title}, opts, runtime_opts) do
    {actor_type, actor_id} = actor(opts)

    create_opts =
      runtime_opts
      |> Keyword.take([:path, :now, :id])
      |> Keyword.put(:intent, Keyword.get(opts, :intent))
      |> Keyword.put(:created_by_type, actor_type)
      |> Keyword.put(:created_by_id, actor_id)

    case Store.create(title, create_opts) do
      {:ok, item} ->
        output_success(%{source: "local", item: item_map(item)}, opts, fn ->
          IO.puts("Created work item #{item.id}")
          IO.puts(item.title)
        end)

      {:error, reason} ->
        output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute(:list, opts, runtime_opts) do
    case Store.list(store_opts(runtime_opts)) do
      {:ok, items} ->
        output_success(%{source: "local", items: Enum.map(items, &item_map/1)}, opts, fn ->
          if items == [] do
            IO.puts("No work items")
          else
            IO.puts("Work items:")

            Enum.each(items, fn item ->
              IO.puts("  #{item.id}  #{item.status}  #{item.title}")
            end)
          end
        end)

      {:error, reason} ->
        output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute({:show, id}, opts, runtime_opts) do
    case Store.get(id, store_opts(runtime_opts)) do
      {:ok, item} ->
        output_success(%{source: "local", item: item_map(item)}, opts, fn ->
          print_item(item)
        end)

      {:error, reason} ->
        output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute({:claim, id}, opts, runtime_opts) do
    {actor_type, actor_id} = actor(opts)

    claim_opts =
      runtime_opts
      |> Keyword.take([:path, :now])

    case Store.claim(id, actor_type, actor_id, claim_opts) do
      {:ok, item} ->
        output_success(%{source: "local", item: item_map(item)}, opts, fn ->
          IO.puts("Claimed work item #{item.id}")
          IO.puts("#{item.assigned_to_type}:#{item.assigned_to_id}")
        end)

      {:error, reason} ->
        output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute({:note, id, body}, opts, runtime_opts) do
    {actor_type, actor_id} = actor(opts)

    note_opts =
      runtime_opts
      |> Keyword.take([:path, :now, :note_id])
      |> Keyword.put(:author_type, actor_type)
      |> Keyword.put(:author_id, actor_id)

    case Store.append_note(id, body, note_opts) do
      {:ok, note, item} ->
        output_success(%{source: "local", note: note, item: item_map(item)}, opts, fn ->
          IO.puts("Added note #{note["id"]} to work item #{item.id}")
        end)

      {:error, reason} ->
        output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute(:help, _opts, _runtime_opts) do
    print_help()
    0
  end

  defp parse(args) do
    {opts, positionals, error} = parse_flags(args, [], [])

    cond do
      error != nil ->
        {:error, error, opts}

      positionals in [[], ["--help"], ["-h"]] ->
        {:ok, :help, opts}

      positionals == ["create"] ->
        {:error, :work_item_missing_title, opts}

      match?(["create" | _], positionals) ->
        ["create" | title_parts] = positionals
        title = title_parts |> Enum.join(" ") |> String.trim()

        if title == "" do
          {:error, :work_item_missing_title, opts}
        else
          {:ok, {:create, title}, opts}
        end

      positionals == ["list"] ->
        {:ok, :list, opts}

      match?(["show", _], positionals) ->
        ["show", id] = positionals
        {:ok, {:show, id}, opts}

      match?(["claim", _], positionals) ->
        ["claim", id] = positionals
        {:ok, {:claim, id}, opts}

      positionals == ["claim"] ->
        {:error, {:invalid_work_command, "claim requires a work item id"}, opts}

      match?(["note", _, _ | _], positionals) ->
        ["note", id | body_parts] = positionals
        body = body_parts |> Enum.join(" ") |> String.trim()

        if body == "" do
          {:error, {:invalid_work_command, "note requires a non-empty body"}, opts}
        else
          {:ok, {:note, id, body}, opts}
        end

      match?(["note"], positionals) or match?(["note", _], positionals) ->
        {:error, {:invalid_work_command, "note requires a work item id and body"}, opts}

      true ->
        {:error, {:invalid_work_command, Enum.join(positionals, " ")}, opts}
    end
  end

  defp parse_flags([], opts, positionals),
    do: {Enum.reverse(opts), Enum.reverse(positionals), nil}

  defp parse_flags(["--json" | rest], opts, positionals) do
    parse_flags(rest, [{:json, true} | opts], positionals)
  end

  defp parse_flags(["--intent", value | rest], opts, positionals) do
    parse_flags(rest, [{:intent, value} | opts], positionals)
  end

  defp parse_flags([<<"--intent=", value::binary>> | rest], opts, positionals) do
    parse_flags(rest, [{:intent, value} | opts], positionals)
  end

  defp parse_flags(["--actor", value | rest], opts, positionals) do
    case parse_actor(value) do
      {:ok, actor_type, actor_id} ->
        parse_flags(rest, [{:actor_type, actor_type}, {:actor_id, actor_id} | opts], positionals)

      {:error, reason} ->
        {Enum.reverse(opts), Enum.reverse(positionals), reason}
    end
  end

  defp parse_flags([<<"--actor=", value::binary>> | rest], opts, positionals) do
    case parse_actor(value) do
      {:ok, actor_type, actor_id} ->
        parse_flags(rest, [{:actor_type, actor_type}, {:actor_id, actor_id} | opts], positionals)

      {:error, reason} ->
        {Enum.reverse(opts), Enum.reverse(positionals), reason}
    end
  end

  defp parse_flags([<<"--", _::binary>> = flag | _rest], opts, positionals) do
    {Enum.reverse(opts), Enum.reverse(positionals), {:unknown_work_flag, flag}}
  end

  defp parse_flags([arg | rest], opts, positionals) do
    parse_flags(rest, opts, [arg | positionals])
  end

  defp parse_actor(value) when is_binary(value) do
    case String.split(value, ":", parts: 2) do
      [actor_type, actor_id] ->
        actor_type = String.trim(actor_type)
        actor_id = String.trim(actor_id)

        with :ok <- WorkItem.validate_actor_type(actor_type),
             :ok <- validate_cli_actor_id(actor_id) do
          {:ok, actor_type, actor_id}
        end

      _ ->
        {:error, {:invalid_work_actor, value}}
    end
  end

  defp validate_cli_actor_id(""), do: {:error, {:invalid_work_actor, :empty_id}}
  defp validate_cli_actor_id(_actor_id), do: :ok

  defp actor(opts) do
    {
      Keyword.get(opts, :actor_type, @default_actor_type),
      Keyword.get(opts, :actor_id, @default_actor_id)
    }
  end

  defp store_opts(runtime_opts), do: Keyword.take(runtime_opts, [:path])

  defp output_success(data, opts, human_fun) do
    if Keyword.get(opts, :json, false) do
      IO.puts(JsonResponse.ok(data))
    else
      human_fun.()
    end

    0
  end

  defp output_error(reason, json_output) do
    error = Error.wrap(reason)

    if json_output do
      IO.puts(JsonResponse.error(error))
    else
      IO.puts(:stderr, Formatter.format(error))
    end

    1
  end

  defp item_map(%WorkItem{} = item), do: WorkItem.to_map(item)

  defp print_item(%WorkItem{} = item) do
    IO.puts("#{item.id}  #{item.status}  #{item.title}")

    if item.intent do
      IO.puts("intent: #{item.intent}")
    end

    if item.assigned_to_type && item.assigned_to_id do
      IO.puts("assigned: #{item.assigned_to_type}:#{item.assigned_to_id}")
    end

    if item.notes != [] do
      IO.puts("notes: #{length(item.notes)}")
    end
  end

  defp print_help do
    IO.puts("""
    Usage: sykli work <command>

    Local work item commands.

    Commands:
      sykli work create "Title" [--intent TEXT] [--actor TYPE:ID]
      sykli work list
      sykli work show <work-id>
      sykli work claim <work-id> [--actor TYPE:ID]
      sykli work note <work-id> "Body" [--actor TYPE:ID]

    Options:
      --json          Output as JSON
      --intent TEXT   Set work item intent when creating
      --actor TYPE:ID Set actor identity, e.g. member:yair, agent:claude, daemon:worker-1
    """)
  end
end
