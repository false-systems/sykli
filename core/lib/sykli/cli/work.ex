defmodule Sykli.CLI.Work do
  @moduledoc """
  Work item CLI commands.

  Commands are local by default. Passing `--team TEAM` routes supported work
  operations through the joined self-hosted Team Mode coordinator.
  """

  alias Sykli.CLI.JsonResponse
  alias Sykli.Daemon.SessionStore
  alias Sykli.Error
  alias Sykli.Error.Formatter
  alias Sykli.TeamCoordinator.WorkClient
  alias Sykli.Work.Store
  alias Sykli.WorkItem

  @default_actor_type "member"

  @doc """
  Runs a `sykli work ...` command and returns the intended process exit code.

  `runtime_opts` are internal test hooks for store path, clocks, ids, and
  default actor attribution. Parsed CLI flags are kept separate in `opts`.
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
    if team_mode?(opts) do
      execute_team_create(title, opts, runtime_opts)
    else
      execute_local_create(title, opts, runtime_opts)
    end
  end

  defp execute(:list, opts, runtime_opts) do
    if team_mode?(opts) do
      execute_team_list(opts, runtime_opts)
    else
      execute_local_list(opts, runtime_opts)
    end
  end

  defp execute({:show, id}, opts, runtime_opts) do
    if team_mode?(opts) do
      execute_team_show(id, opts, runtime_opts)
    else
      execute_local_show(id, opts, runtime_opts)
    end
  end

  defp execute({:claim, id}, opts, runtime_opts) do
    if team_mode?(opts) do
      execute_team_claim(id, opts, runtime_opts)
    else
      execute_local_claim(id, opts, runtime_opts)
    end
  end

  defp execute({:note, id, body}, opts, runtime_opts) do
    if team_mode?(opts) do
      execute_team_note(id, body, opts, runtime_opts)
    else
      execute_local_note(id, body, opts, runtime_opts)
    end
  end

  defp execute({:runs, id}, opts, runtime_opts) do
    if team_mode?(opts) do
      output_error(:team_work_runs_not_supported, Keyword.get(opts, :json, false))
    else
      execute_local_runs(id, opts, runtime_opts)
    end
  end

  defp execute(:help, _opts, _runtime_opts) do
    print_help()
    0
  end

  defp execute_local_create(title, opts, runtime_opts) do
    {actor_type, actor_id} = actor(opts, runtime_opts)

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

  defp execute_local_list(opts, runtime_opts) do
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

  defp execute_local_show(id, opts, runtime_opts) do
    case Store.get(id, store_opts(runtime_opts)) do
      {:ok, item} ->
        output_success(%{source: "local", item: item_map(item)}, opts, fn ->
          print_item(item)
        end)

      {:error, reason} ->
        output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute_local_claim(id, opts, runtime_opts) do
    {actor_type, actor_id} = actor(opts, runtime_opts)

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

  defp execute_local_note(id, body, opts, runtime_opts) do
    {actor_type, actor_id} = actor(opts, runtime_opts)

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

  defp execute_local_runs(id, opts, runtime_opts) do
    store_opts = store_opts(runtime_opts)
    history_opts = Keyword.merge(store_opts, Keyword.take(runtime_opts, [:limit]))

    with {:ok, _item} <- Store.get(id, store_opts),
         {:ok, runs} <- Sykli.RunHistory.list_by_work_item(id, history_opts) do
      output_success(
        %{source: "local", work_item_id: id, runs: Enum.map(runs, &run_map/1)},
        opts,
        fn ->
          if runs == [] do
            IO.puts("No runs for work item #{id}")
          else
            IO.puts("Runs for work item #{id}:")

            Enum.each(runs, fn run ->
              IO.puts("  #{run.id}  #{run.overall}  #{run.contract_hash}")
            end)
          end
        end
      )
    else
      {:error, reason} ->
        output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute_team_create(title, opts, runtime_opts) do
    {actor_type, actor_id} = actor(opts, runtime_opts)

    attrs =
      %{"title" => title, "created_by" => "#{actor_type}:#{actor_id}"}
      |> maybe_put("intent", Keyword.get(opts, :intent))

    with {:ok, session, token} <- team_session(opts, runtime_opts),
         {:ok, item} <-
           work_client(runtime_opts).create(session, token, attrs, client_opts(runtime_opts)) do
      output_success(%{source: "team", team: session["team"], item: item}, opts, fn ->
        IO.puts("Created team work item #{item["id"]}")
        IO.puts(item["title"])
      end)
    else
      {:error, reason} -> output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute_team_list(opts, runtime_opts) do
    with {:ok, session, token} <- team_session(opts, runtime_opts),
         {:ok, items} <- work_client(runtime_opts).list(session, token, client_opts(runtime_opts)) do
      output_success(%{source: "team", team: session["team"], items: items}, opts, fn ->
        if items == [] do
          IO.puts("No team work items")
        else
          IO.puts("Team work items:")

          Enum.each(items, fn item ->
            IO.puts("  #{item["id"]}  #{item["status"]}  #{item["title"]}")
          end)
        end
      end)
    else
      {:error, reason} -> output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute_team_show(id, opts, runtime_opts) do
    with {:ok, session, token} <- team_session(opts, runtime_opts),
         {:ok, item} <-
           work_client(runtime_opts).show(session, token, id, client_opts(runtime_opts)) do
      output_success(%{source: "team", team: session["team"], item: item}, opts, fn ->
        print_remote_item(item)
      end)
    else
      {:error, reason} -> output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute_team_claim(id, opts, runtime_opts) do
    {actor_type, actor_id} = actor(opts, runtime_opts)

    attrs = %{
      "assigned_to_type" => actor_type,
      "assigned_to_id" => actor_id
    }

    with {:ok, session, token} <- team_session(opts, runtime_opts),
         {:ok, item} <-
           work_client(runtime_opts).claim(session, token, id, attrs, client_opts(runtime_opts)) do
      output_success(%{source: "team", team: session["team"], item: item}, opts, fn ->
        IO.puts("Claimed team work item #{item["id"]}")
        IO.puts("#{item["assigned_to_type"]}:#{item["assigned_to_id"]}")
      end)
    else
      {:error, reason} -> output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute_team_note(id, body, opts, runtime_opts) do
    {actor_type, actor_id} = actor(opts, runtime_opts)

    attrs = %{
      "body" => body,
      "author_type" => actor_type,
      "author_id" => actor_id
    }

    with {:ok, session, token} <- team_session(opts, runtime_opts),
         {:ok, note} <-
           work_client(runtime_opts).note(session, token, id, attrs, client_opts(runtime_opts)) do
      output_success(%{source: "team", team: session["team"], note: note}, opts, fn ->
        IO.puts("Added note #{note["id"]} to team work item #{id}")
      end)
    else
      {:error, reason} -> output_error(reason, Keyword.get(opts, :json, false))
    end
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

      match?(["runs", _], positionals) ->
        ["runs", id] = positionals
        {:ok, {:runs, id}, opts}

      positionals == ["runs"] ->
        {:error, {:invalid_work_command, "runs requires a work item id"}, opts}

      true ->
        {:error, {:invalid_work_command, Enum.join(positionals, " ")}, opts}
    end
  end

  defp parse_flags([], opts, positionals),
    do: {Enum.reverse(opts), Enum.reverse(positionals), nil}

  defp parse_flags(["--json" | rest], opts, positionals) do
    parse_flags(rest, [{:json, true} | opts], positionals)
  end

  defp parse_flags(["--help" | rest], opts, positionals) do
    parse_flags(rest, opts, ["--help" | positionals])
  end

  defp parse_flags(["-h" | rest], opts, positionals) do
    parse_flags(rest, opts, ["-h" | positionals])
  end

  defp parse_flags(["--intent", value | rest], opts, positionals) do
    parse_flags(rest, [{:intent, value} | opts], positionals)
  end

  defp parse_flags([<<"--intent=", value::binary>> | rest], opts, positionals) do
    parse_flags(rest, [{:intent, value} | opts], positionals)
  end

  defp parse_flags(["--team", value | rest], opts, positionals) do
    parse_flags(rest, [{:team, value} | opts], positionals)
  end

  defp parse_flags([<<"--team=", value::binary>> | rest], opts, positionals) do
    parse_flags(rest, [{:team, value} | opts], positionals)
  end

  defp parse_flags(["--actor", value | rest], opts, positionals) do
    case parse_actor(value) do
      {:ok, actor_type, actor_id} ->
        parse_flags(rest, [{:actor_type, actor_type}, {:actor_id, actor_id} | opts], positionals)

      {:error, reason} ->
        finalize_parse_error(opts, positionals, rest, reason)
    end
  end

  defp parse_flags([<<"--actor=", value::binary>> | rest], opts, positionals) do
    case parse_actor(value) do
      {:ok, actor_type, actor_id} ->
        parse_flags(rest, [{:actor_type, actor_type}, {:actor_id, actor_id} | opts], positionals)

      {:error, reason} ->
        finalize_parse_error(opts, positionals, rest, reason)
    end
  end

  defp parse_flags([<<"--", _::binary>> = flag | rest], opts, positionals) do
    finalize_parse_error(opts, positionals, rest, {:unknown_work_flag, flag})
  end

  defp parse_flags([arg | rest], opts, positionals) do
    parse_flags(rest, opts, [arg | positionals])
  end

  defp finalize_parse_error(opts, positionals, rest, reason) do
    opts =
      if "--json" in rest do
        [{:json, true} | opts]
      else
        opts
      end

    {Enum.reverse(opts), Enum.reverse(positionals), reason}
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

  defp actor(opts, runtime_opts) do
    {default_type, default_id} = default_actor(runtime_opts)

    {
      Keyword.get(opts, :actor_type, default_type),
      Keyword.get(opts, :actor_id, default_id)
    }
  end

  defp default_actor(runtime_opts) do
    {
      Keyword.get(runtime_opts, :default_actor_type, @default_actor_type),
      Keyword.get_lazy(runtime_opts, :default_actor_id, &default_actor_id/0)
    }
  end

  defp default_actor_id do
    ["SYKLI_ACTOR_ID", "USER", "USERNAME"]
    |> Enum.find_value(fn name ->
      case System.get_env(name) do
        value when is_binary(value) and value != "" -> value
        _ -> nil
      end
    end)
    |> case do
      nil -> "local"
      value -> value
    end
  end

  defp store_opts(runtime_opts), do: Keyword.take(runtime_opts, [:path])

  defp team_mode?(opts), do: Keyword.has_key?(opts, :team)

  defp team_session(opts, runtime_opts) do
    requested_team = Keyword.get(opts, :team)

    with {:ok, session} <-
           session_store(runtime_opts).read(path: Keyword.get(runtime_opts, :path)),
         :ok <- validate_requested_team(session, requested_team),
         {:ok, token} <- team_token(runtime_opts) do
      {:ok, session, token}
    end
  end

  defp validate_requested_team(_session, team) when team in [nil, ""],
    do: {:error, :team_work_team_required}

  defp validate_requested_team(%{"team" => team}, team), do: :ok

  defp validate_requested_team(%{"team" => joined_team}, requested_team),
    do: {:error, {:team_work_team_mismatch, requested_team, joined_team}}

  defp team_token(runtime_opts) do
    case Keyword.get(runtime_opts, :team_token) || System.get_env("SYKLI_TEAM_TOKEN") do
      value when is_binary(value) and value != "" -> {:ok, value}
      _ -> {:error, :team_work_missing_token}
    end
  end

  defp session_store(runtime_opts), do: Keyword.get(runtime_opts, :session_store, SessionStore)
  defp work_client(runtime_opts), do: Keyword.get(runtime_opts, :work_client, WorkClient)
  defp client_opts(runtime_opts), do: Keyword.take(runtime_opts, [:client])

  defp maybe_put(map, _key, nil), do: map
  defp maybe_put(map, _key, ""), do: map
  defp maybe_put(map, key, value), do: Map.put(map, key, value)

  defp output_success(data, opts, human_fun) do
    if Keyword.get(opts, :json, false) do
      IO.puts(JsonResponse.ok(data))
    else
      human_fun.()
    end

    0
  end

  defp output_error(reason, json_output) do
    error = work_error(reason)

    if json_output do
      IO.puts(JsonResponse.error(error))
    else
      IO.puts(:stderr, Formatter.format(error))
    end

    1
  end

  defp work_error(:daemon_session_not_found),
    do: team_error("work.team_not_joined", "team work requires `sykli daemon join` first")

  defp work_error(:daemon_session_malformed),
    do: team_error("work.team_session_invalid", "local daemon coordinator session is malformed")

  defp work_error(:daemon_session_invalid),
    do: team_error("work.team_session_invalid", "local daemon coordinator session is invalid")

  defp work_error(:team_work_missing_token),
    do: team_error("work.team_missing_token", "team work requires SYKLI_TEAM_TOKEN")

  defp work_error(:team_work_team_required),
    do: team_error("work.team_required", "team mode requires --team <team>")

  defp work_error({:team_work_team_mismatch, requested, joined}),
    do:
      team_error(
        "work.team_mismatch",
        "requested team #{inspect(requested)} does not match joined team #{inspect(joined)}"
      )

  defp work_error(:team_work_runs_not_supported),
    do:
      team_error(
        "work.team_runs_not_supported",
        "`sykli work runs --team` is not available until run summary sync"
      )

  defp work_error(:team_unauthorized),
    do:
      team_error(
        "work.team_unauthorized",
        "coordinator rejected team work authorization"
      )

  defp work_error({:team_coordinator_unavailable, reason}),
    do:
      team_error(
        "work.team_coordinator_unavailable",
        "coordinator unavailable: #{inspect(reason)}"
      )

  defp work_error(:team_invalid_coordinator_response),
    do: team_error("work.team_invalid_coordinator_response", "coordinator returned invalid JSON")

  defp work_error({:team_invalid_coordinator_response, _data}),
    do:
      team_error(
        "work.team_invalid_coordinator_response",
        "coordinator returned an unexpected JSON shape"
      )

  defp work_error({:team_coordinator_error, %{"code" => code, "message" => message} = error}) do
    %Error{
      code: code,
      type: :runtime,
      message: message,
      step: :setup,
      hints: Map.get(error, "hints", [])
    }
  end

  defp work_error({:team_coordinator_error, error}),
    do:
      team_error(
        "work.team_coordinator_error",
        "coordinator rejected team work request: #{inspect(error)}"
      )

  defp work_error(reason), do: Error.wrap(reason)

  defp team_error(code, message) do
    %Error{
      code: code,
      type: :runtime,
      message: message,
      step: :setup,
      hints: []
    }
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

  defp print_remote_item(item) do
    IO.puts("#{item["id"]}  #{item["status"]}  #{item["title"]}")

    if item["intent"] do
      IO.puts("intent: #{item["intent"]}")
    end

    if item["assigned_to_type"] && item["assigned_to_id"] do
      IO.puts("assigned: #{item["assigned_to_type"]}:#{item["assigned_to_id"]}")
    end
  end

  defp print_help do
    IO.puts("""
    Usage: sykli work <command>

    Local work item commands by default. Add `--team TEAM` to use a joined
    self-hosted coordinator for shared team work items.

    Unquoted title/body words are joined with spaces. Unknown flags are rejected.

    Commands:
      sykli work create "Title" [--intent TEXT] [--actor TYPE:ID] [--team TEAM]
      sykli work list [--team TEAM]
      sykli work show <work-id> [--team TEAM]
      sykli work claim <work-id> [--actor TYPE:ID] [--team TEAM]
      sykli work note <work-id> "Body" [--actor TYPE:ID] [--team TEAM]
      sykli work runs <work-id>

    Options:
      --json          Output as JSON
      --team TEAM     Use the joined coordinator team instead of local .sykli state
      --intent TEXT   Set work item intent when creating
      --actor TYPE:ID Set actor identity, e.g. member:yair, agent:claude, daemon:worker-1
    """)
  end

  defp run_map(run) do
    %{
      id: run.id,
      work_item_id: run.work_item_id,
      contract_hash: run.contract_hash,
      status: Atom.to_string(run.overall),
      timestamp: DateTime.to_iso8601(run.timestamp),
      task_count: length(run.tasks),
      passed_count: Enum.count(run.tasks, &(&1.status == :passed)),
      failed_count: Enum.count(run.tasks, &(&1.status in [:failed, :errored]))
    }
  end
end
