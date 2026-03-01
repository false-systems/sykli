defmodule Sykli.Query.Coverage do
  @moduledoc """
  Coverage queries: which tasks cover which files/patterns.
  """

  alias Sykli.Graph.Task

  @spec execute(String.t(), String.t()) :: {:ok, map()} | {:error, term()}
  def execute(pattern, path) do
    with {:ok, graph} <- load_graph(path) do
      tasks = find_covering_tasks(pattern, graph)

      {:ok, %{
        type: :coverage,
        data: %{
          pattern: pattern,
          tasks: tasks,
          total: length(tasks)
        },
        metadata: metadata("coverage for #{pattern}")
      }}
    end
  end

  defp find_covering_tasks(pattern, graph) do
    graph
    |> Enum.filter(fn {_name, task} ->
      semantic = Task.semantic(task)
      covers = semantic.covers || []
      Enum.any?(covers, &pattern_match?(pattern, &1))
    end)
    |> Enum.map(fn {name, task} ->
      semantic = Task.semantic(task)

      %{
        name: name,
        covers: semantic.covers || [],
        intent: semantic.intent,
        criticality: if(semantic.criticality, do: Atom.to_string(semantic.criticality))
      }
    end)
    |> Enum.sort_by(& &1.name)
  end

  # Check if a user pattern matches a task's cover glob.
  # "auth" matches "src/auth/*" (substring)
  # "src/auth/login.ts" matches "src/auth/*" (glob)
  defp pattern_match?(user_pattern, cover_glob) do
    String.contains?(cover_glob, user_pattern) or glob_match?(user_pattern, cover_glob)
  end

  defp glob_match?(path, glob) do
    regex_str =
      glob
      |> Regex.escape()
      |> String.replace("\\*\\*/", "(.*/)?")
      |> String.replace("\\*\\*", ".*")
      |> String.replace("\\*", "[^/]*")

    case Regex.compile("^#{regex_str}$") do
      {:ok, regex} -> Regex.match?(regex, path)
      _ -> false
    end
  end

  defp load_graph(path) do
    alias Sykli.{Detector, Graph}

    with {:ok, sdk_file} <- Detector.find(path),
         {:ok, json} <- Detector.emit(sdk_file),
         {:ok, graph} <- Graph.parse(json) do
      {:ok, Graph.expand_matrix(graph)}
    end
  end

  defp metadata(query) do
    %{query: query, timestamp: DateTime.utc_now() |> DateTime.to_iso8601()}
  end
end
