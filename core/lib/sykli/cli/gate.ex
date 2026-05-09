defmodule Sykli.CLI.Gate do
  @moduledoc """
  Local gate decision CLI commands.

  This module is intentionally local-only. Coordinator gate sync is added in a
  later Team Mode phase.
  """

  alias Sykli.CLI.JsonResponse
  alias Sykli.Error
  alias Sykli.Error.Formatter
  alias Sykli.Gate.Store
  alias Sykli.GateDecision

  @default_actor_type "member"

  @doc """
  Runs a `sykli gate ...` or `sykli gates ...` command and returns the intended
  process exit code.

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

  defp execute(:list, opts, runtime_opts) do
    store_opts = store_opts(runtime_opts)

    result =
      case Keyword.get(opts, :status) do
        nil -> Store.list(store_opts)
        status -> Store.list_by_status(status, store_opts)
      end

    case result do
      {:ok, gates} ->
        output_success(%{source: "local", gates: Enum.map(gates, &gate_map/1)}, opts, fn ->
          if gates == [] do
            IO.puts("No gates")
          else
            IO.puts("Gates:")

            Enum.each(gates, fn gate ->
              IO.puts("  #{gate.id}  #{gate.status}  #{gate.node_id || "-"}")
            end)
          end
        end)

      {:error, reason} ->
        output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute({:show, id}, opts, runtime_opts) do
    case Store.get(id, store_opts(runtime_opts)) do
      {:ok, gate} ->
        output_success(%{source: "local", gate: gate_map(gate)}, opts, fn ->
          print_gate(gate)
        end)

      {:error, reason} ->
        output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp execute({:approve, id}, opts, runtime_opts) do
    decide(:approve, id, opts, runtime_opts)
  end

  defp execute({:reject, id}, opts, runtime_opts) do
    decide(:reject, id, opts, runtime_opts)
  end

  defp execute(:help, _opts, _runtime_opts) do
    print_help()
    0
  end

  defp decide(action, id, opts, runtime_opts) do
    reason = Keyword.get(opts, :reason)
    {actor_type, actor_id} = actor(opts, runtime_opts)

    decision_opts =
      runtime_opts
      |> Keyword.take([:path, :now])
      |> Keyword.put(:decided_by, "#{actor_type}:#{actor_id}")

    result =
      case action do
        :approve -> Store.approve(id, reason, decision_opts)
        :reject -> Store.reject(id, reason, decision_opts)
      end

    case result do
      {:ok, gate} ->
        verb = if action == :approve, do: "Approved", else: "Rejected"

        output_success(%{source: "local", gate: gate_map(gate)}, opts, fn ->
          IO.puts("#{verb} gate #{gate.id}")
        end)

      {:error, reason} ->
        output_error(reason, Keyword.get(opts, :json, false))
    end
  end

  defp parse(args) do
    {opts, positionals, error} = parse_flags(args, [], [])

    cond do
      error != nil ->
        {:error, error, opts}

      positionals in [[], ["--help"], ["-h"]] ->
        {:ok, :help, opts}

      positionals == ["list"] ->
        {:ok, :list, opts}

      match?(["show", _], positionals) ->
        ["show", id] = positionals
        {:ok, {:show, id}, opts}

      positionals == ["show"] ->
        {:error, {:invalid_gate_command, "show requires a gate id"}, opts}

      match?(["approve", _], positionals) ->
        ["approve", id] = positionals
        {:ok, {:approve, id}, opts}

      positionals == ["approve"] ->
        {:error, {:invalid_gate_command, "approve requires a gate id"}, opts}

      match?(["reject", _], positionals) ->
        ["reject", id] = positionals
        {:ok, {:reject, id}, opts}

      positionals == ["reject"] ->
        {:error, {:invalid_gate_command, "reject requires a gate id"}, opts}

      true ->
        {:error, {:invalid_gate_command, Enum.join(positionals, " ")}, opts}
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

  defp parse_flags(["--reason", value | rest], opts, positionals) do
    parse_flags(rest, [{:reason, value} | opts], positionals)
  end

  defp parse_flags([<<"--reason=", value::binary>> | rest], opts, positionals) do
    parse_flags(rest, [{:reason, value} | opts], positionals)
  end

  defp parse_flags(["--status", value | rest], opts, positionals) do
    parse_flags(rest, [{:status, value} | opts], positionals)
  end

  defp parse_flags([<<"--status=", value::binary>> | rest], opts, positionals) do
    parse_flags(rest, [{:status, value} | opts], positionals)
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
    finalize_parse_error(opts, positionals, rest, {:unknown_gate_flag, flag})
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

        with :ok <- validate_actor_type(actor_type),
             :ok <- validate_cli_actor_id(actor_id) do
          {:ok, actor_type, actor_id}
        end

      _ ->
        {:error, {:invalid_gate_actor, value}}
    end
  end

  defp validate_actor_type(type) when type in ~w(member agent daemon system), do: :ok
  defp validate_actor_type(type), do: {:error, {:invalid_gate_requester_type, type}}

  defp validate_cli_actor_id(""), do: {:error, {:invalid_gate_actor, :empty_id}}
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

  defp gate_map(%GateDecision{} = gate), do: GateDecision.to_map(gate)

  defp print_gate(%GateDecision{} = gate) do
    IO.puts("#{gate.id}  #{gate.status}")

    if gate.work_item_id, do: IO.puts("work_item_id: #{gate.work_item_id}")
    if gate.run_id, do: IO.puts("run_id: #{gate.run_id}")
    if gate.node_id, do: IO.puts("node_id: #{gate.node_id}")
    if gate.reason, do: IO.puts("reason: #{gate.reason}")
    if gate.decided_by, do: IO.puts("decided_by: #{gate.decided_by}")
  end

  defp print_help do
    IO.puts("""
    Usage: sykli gate <command>
           sykli gates list

    Local gate decision commands.

    Unknown flags are rejected. Approval and rejection require --reason.

    Commands:
      sykli gates list [--status STATUS]
      sykli gate show <gate-id>
      sykli gate approve <gate-id> --reason TEXT [--actor TYPE:ID]
      sykli gate reject <gate-id> --reason TEXT [--actor TYPE:ID]

    Options:
      --json          Output as JSON
      --status STATUS Filter list output by gate status
      --reason TEXT   Decision reason for approve/reject
      --actor TYPE:ID Set actor identity, e.g. member:yair, agent:claude, daemon:worker-1
    """)
  end
end
