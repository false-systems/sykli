defmodule Sykli.TeamCoordinator.RunSummary do
  @moduledoc """
  Small Team Mode projection of a local run.

  The summary is intentionally metadata-only: no logs, source, artifacts, or
  contract bytes are included.
  """

  alias Sykli.RunHistory

  @version "1"

  @enforce_keys [:run]
  defstruct version: @version,
            run: %{},
            nodes: [],
            criteria_results: [],
            review_results: [],
            gates: [],
            evidence_refs: []

  def from_run(%RunHistory.Run{} = run, opts) do
    session = Keyword.get(opts, :session, opts)
    path = Keyword.get(opts, :path, ".")
    total_duration_ms = total_duration_ms(run)
    finished_at = run.timestamp

    started_at =
      if total_duration_ms > 0,
        do: DateTime.add(finished_at, -total_duration_ms, :millisecond),
        else: finished_at

    %__MODULE__{
      run: %{
        "id" => run.id,
        "org_slug" => session["org"] || session["org_slug"],
        "team_slug" => session["team"] || session["team_slug"],
        "daemon_session_id" => session["session_id"] || session["daemon_session_id"],
        "work_item_id" => run.work_item_id,
        "contract_hash" => run.contract_hash,
        "status" => Atom.to_string(run.overall),
        "error_code" => error_code(run),
        "target" => run.platform || "local",
        "started_at" => DateTime.to_iso8601(started_at),
        "finished_at" => DateTime.to_iso8601(finished_at),
        "total_duration_ms" => total_duration_ms,
        "git_ref" => run.git_ref,
        "git_branch" => run.git_branch
      },
      nodes: Enum.map(run.tasks, &run_node/1),
      criteria_results: Enum.flat_map(run.tasks, &criteria_results/1),
      review_results: Enum.flat_map(run.tasks, &review_results/1),
      gates: gates(run, path),
      evidence_refs: evidence_refs(run, path)
    }
  end

  def encode(%__MODULE__{} = summary) do
    %{
      "version" => summary.version,
      "run" => summary.run,
      "nodes" => summary.nodes,
      "criteria_results" => summary.criteria_results,
      "review_results" => summary.review_results,
      "gates" => summary.gates,
      "evidence_refs" => summary.evidence_refs
    }
  end

  defp run_node(%RunHistory.TaskResult{} = task) do
    %{
      "name" => task.name,
      "kind" => task.kind || "task",
      "status" => node_status(task),
      "error_code" => task_error_code(task),
      "duration_ms" => task.duration_ms
    }
  end

  defp node_status(%RunHistory.TaskResult{cached: true}), do: "cached"
  defp node_status(%RunHistory.TaskResult{status: status}), do: Atom.to_string(status)

  defp total_duration_ms(%RunHistory.Run{tasks: tasks}) do
    tasks
    |> Enum.map(&(&1.duration_ms || 0))
    |> Enum.sum()
    |> max(0)
  end

  defp error_code(%RunHistory.Run{overall: :passed}), do: nil

  defp error_code(%RunHistory.Run{tasks: tasks}) do
    tasks
    |> Enum.find_value(&task_error_code/1)
  end

  defp task_error_code(%RunHistory.TaskResult{error: nil}), do: nil

  defp task_error_code(%RunHistory.TaskResult{error: error}) when is_binary(error) do
    error |> String.split(":", parts: 2) |> List.first()
  end

  defp criteria_results(%RunHistory.TaskResult{} = task) do
    task.success_criteria_results
    |> List.wrap()
    |> Enum.map(fn result ->
      %{
        "task" => task.name,
        "type" => result["type"],
        "status" => result["status"],
        "message" => result["message"]
      }
    end)
  end

  defp review_results(%RunHistory.TaskResult{review_result: nil}), do: []

  defp review_results(%RunHistory.TaskResult{name: name, review_result: result}) do
    [
      %{
        "task" => name,
        "review_type" => result["review_type"],
        "status" => result["status"],
        "severity" => result["severity"],
        "message" => result["message"],
        "tool" => result["tool"]
      }
    ]
  end

  defp gates(run, path) do
    case Sykli.Gate.Store.list(path: path) do
      {:ok, gates} ->
        gates
        |> Enum.filter(&(&1.run_id == run.id))
        |> Enum.map(fn gate ->
          %{"id" => gate.id, "status" => gate.status, "decided_by" => gate.decided_by}
        end)

      {:error, _reason} ->
        []
    end
  end

  defp evidence_refs(run, path) do
    occurrence = Path.expand(Path.join([path, ".sykli", "occurrence.json"]))

    if File.exists?(occurrence) do
      [
        %{
          "type" => "occurrence",
          "uri" => "file://" <> occurrence,
          "hash" => file_hash(occurrence),
          "summary" => "run #{run.id} #{Atom.to_string(run.overall)}",
          "visibility" => "local_only"
        }
      ]
    else
      []
    end
  end

  defp file_hash(path) do
    hash = path |> File.read!() |> then(&:crypto.hash(:sha256, &1)) |> Base.encode16(case: :lower)
    "sha256:" <> hash
  end
end
