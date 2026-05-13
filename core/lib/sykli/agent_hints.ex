defmodule Sykli.AgentHints do
  @moduledoc """
  Minimal machine-readable hints derived from failure semantics.

  These hints are intentionally factual and conservative. They do not diagnose
  root cause; they only expose which follow-up paths are supported by the
  normalized failure class.
  """

  alias Sykli.FailureSemantics

  @empty %{
    "retry_may_help" => false,
    "inspect_target" => false,
    "inspect_contract" => false,
    "inspect_dependencies" => false,
    "requires_human_decision" => false
  }

  @type t :: %{
          required(String.t()) => boolean()
        }

  @spec from_failure_semantics(FailureSemantics.t() | map() | nil) :: t() | nil
  def from_failure_semantics(nil), do: nil

  def from_failure_semantics(%FailureSemantics{} = semantics) do
    from_map(FailureSemantics.to_map(semantics))
  end

  def from_failure_semantics(map) when is_map(map), do: from_map(map)

  defp from_map(%{"class" => "runtime_failure"} = semantics) do
    @empty
    |> Map.put("retry_may_help", retryable?(semantics))
    |> Map.put("inspect_target", true)
  end

  defp from_map(%{"class" => "criteria_failure"}) do
    Map.put(@empty, "inspect_contract", true)
  end

  defp from_map(%{"class" => "contract_failure"}) do
    Map.put(@empty, "inspect_contract", true)
  end

  defp from_map(%{"class" => "unsupported_target"}) do
    @empty
    |> Map.put("inspect_target", true)
    |> Map.put("inspect_contract", true)
  end

  defp from_map(%{"class" => "timeout"} = semantics) do
    @empty
    |> Map.put("retry_may_help", retryable?(semantics))
    |> Map.put("inspect_target", true)
  end

  defp from_map(%{"class" => "dependency_failure"}) do
    Map.put(@empty, "inspect_dependencies", true)
  end

  defp from_map(%{"class" => "policy_block"}) do
    Map.put(@empty, "requires_human_decision", true)
  end

  defp from_map(%{"class" => "missing_evidence"}) do
    Map.put(@empty, "inspect_contract", true)
  end

  defp from_map(%{"class" => "agent_variance_failure"}) do
    Map.put(@empty, "inspect_contract", true)
  end

  defp from_map(_map), do: @empty

  defp retryable?(%{"retryable" => true}), do: true
  defp retryable?(_semantics), do: false
end
