defmodule Sykli.FailureSemantics do
  @moduledoc """
  Normalized task result/failure classification.

  This is intentionally independent from the pipeline contract schema. It is
  produced by the executor and persisted in history/occurrences so agents can
  distinguish runtime failures, contract failures, unsupported targets, policy
  blocks, and skipped work without reverse-engineering error strings.
  """

  # Keep @classes and the failure_class typespec in lockstep. `from_map/1`
  # filters parsed atoms against this list, so a class missing here degrades to
  # :unknown even if the type mentions it.
  @classes ~w(
    runtime_failure
    contract_failure
    criteria_failure
    unsupported_target
    timeout
    dependency_failure
    policy_block
    skipped
    internal_error
    unknown
    missing_evidence
    agent_variance_failure
  )a

  @sources ~w(executor target criteria gate dependency system unknown)a

  @enforce_keys [:class, :retryable, :source, :reason, :message]
  defstruct [:class, :retryable, :source, :reason, :message, details: %{}]

  @type failure_class ::
          :runtime_failure
          | :contract_failure
          | :criteria_failure
          | :unsupported_target
          | :timeout
          | :dependency_failure
          | :policy_block
          | :skipped
          | :internal_error
          | :unknown
          | :missing_evidence
          | :agent_variance_failure

  @type source :: :executor | :target | :criteria | :gate | :dependency | :system | :unknown

  @type t :: %__MODULE__{
          class: failure_class(),
          retryable: boolean(),
          source: source(),
          reason: String.t(),
          message: String.t(),
          details: map()
        }

  def runtime_failure(reason, message, details \\ %{}) do
    new(:runtime_failure, false, :target, reason, message, details)
  end

  def criteria_failure(reason, message, details \\ %{}) do
    new(:criteria_failure, false, :criteria, reason, message, details)
  end

  def contract_failure(reason, message, details \\ %{}) do
    new(:contract_failure, false, :executor, reason, message, details)
  end

  def unsupported_target(reason, message, details \\ %{}) do
    new(:unsupported_target, false, :target, reason, message, details)
  end

  def timeout(reason, message, details \\ %{}) do
    new(:timeout, true, :target, reason, message, details)
  end

  def dependency_failure(reason, message, details \\ %{}) do
    new(:dependency_failure, false, :dependency, reason, message, details)
  end

  def policy_block(reason, message, details \\ %{}) do
    new(:policy_block, false, :gate, reason, message, details)
  end

  def skipped(reason, message, details \\ %{}) do
    new(:skipped, false, :executor, reason, message, details)
  end

  def internal_error(reason, message, details \\ %{}) do
    new(:internal_error, false, :system, reason, message, details)
  end

  def unknown(reason, message, details \\ %{}) do
    new(:unknown, false, :unknown, reason, message, details)
  end

  def missing_evidence(reason, message, details \\ %{}) do
    new(:missing_evidence, false, :criteria, reason, message, details)
  end

  # Reserved for the agent-variance contract work. V1 can deserialize it from
  # future/local records, but no executor path emits it yet.
  def agent_variance_failure(reason, message, details \\ %{}) do
    new(:agent_variance_failure, false, :executor, reason, message, details)
  end

  @doc "Returns nil for successful/cached task results; otherwise classifies the result."
  def for_result(:passed, _error), do: nil
  def for_result(:cached, _error), do: nil
  def for_result(:skipped, nil), do: skipped("condition_not_met", "task skipped")

  def for_result(:skipped, reason),
    do: skipped(to_reason(reason), message(reason, "task skipped"))

  def for_result(:blocked, :dependency_failed),
    do: dependency_failure("dependency_failed", "blocked by failed dependency")

  def for_result(:blocked, reason),
    do: dependency_failure(to_reason(reason), message(reason, "task blocked by dependency"))

  # Error structs carry the most precise code/type/step data; classify them via
  # for_error/1 before any generic status fallback.
  def for_result(_status, %Sykli.Error{} = error), do: for_error(error)

  def for_result(:errored, reason),
    do: internal_error(to_reason(reason), message(reason, "task errored"))

  def for_result(:failed, reason), do: unknown(to_reason(reason), message(reason, "task failed"))

  def for_result(_status, reason),
    do: unknown(to_reason(reason), message(reason, "task did not complete successfully"))

  def for_error(%Sykli.Error{code: "task_failed"} = error) do
    runtime_failure(
      "command_failed",
      error.message || "task command failed",
      error_details(error)
    )
  end

  def for_error(%Sykli.Error{code: "success_criteria_failed"} = error) do
    criteria_failure(
      "success_criteria_failed",
      error.message || "success criteria failed",
      error_details(error)
    )
  end

  def for_error(%Sykli.Error{code: "unsupported_success_criteria_for_target"} = error) do
    unsupported_target(
      "unsupported_success_criteria",
      error.message || "target cannot evaluate success criteria",
      error_details(error)
    )
  end

  def for_error(%Sykli.Error{code: "task_timeout"} = error) do
    timeout("task_timeout", error.message || "task timed out", error_details(error))
  end

  def for_error(%Sykli.Error{code: "review_primitive_failed"} = error) do
    contract_failure(
      "review_primitive_failed",
      error.message || "review primitive failed",
      error_details(error)
    )
  end

  def for_error(%Sykli.Error{code: "missing_secrets"} = error) do
    dependency_failure(
      "missing_secrets",
      error.message || "required secrets were missing",
      error_details(error)
    )
  end

  def for_error(%Sykli.Error{type: :internal} = error) do
    internal_error(
      error.code || "internal_error",
      error.message || "internal error",
      error_details(error)
    )
  end

  def for_error(%Sykli.Error{} = error) do
    unknown(
      error.code || "unknown",
      error.message || "unclassified task failure",
      error_details(error)
    )
  end

  @doc """
  Serializes failure semantics to a string-keyed map suitable for JSON output.

  Accepts `nil` (passthrough), a `%FailureSemantics{}` struct (canonical
  serialization), or an already-serialized map (passthrough — useful when a
  value has been round-tripped through `from_map/1` and back, or when a caller
  is unsure of the input shape).
  """
  def to_map(nil), do: nil

  def to_map(%__MODULE__{} = semantics) do
    %{
      "class" => Atom.to_string(semantics.class),
      "retryable" => semantics.retryable,
      "source" => Atom.to_string(semantics.source),
      "reason" => semantics.reason,
      "message" => semantics.message
    }
    |> maybe_put("details", non_empty_map(semantics.details))
  end

  def to_map(map) when is_map(map), do: map

  def from_map(nil), do: nil

  def from_map(map) when is_map(map) do
    class = parse_atom(map["class"], @classes, :unknown)
    source = parse_atom(map["source"], @sources, :unknown)

    %__MODULE__{
      class: class,
      retryable: map["retryable"] == true,
      source: source,
      reason: string_or_default(map["reason"], "unknown"),
      message: string_or_default(map["message"], "unclassified task result"),
      details: map["details"] || %{}
    }
  end

  defp new(class, retryable, source, reason, message, details) do
    %__MODULE__{
      class: class,
      retryable: retryable,
      source: source,
      reason: to_reason(reason),
      message: message,
      details: stringify_keys(details || %{})
    }
  end

  defp error_details(%Sykli.Error{} = error) do
    %{}
    |> maybe_put("code", error.code)
    |> maybe_put("task", error.task)
    |> maybe_put("step", atom_to_string(error.step))
    |> maybe_put("exit_code", error.exit_code)
    |> maybe_put("duration_ms", error.duration_ms)
  end

  defp to_reason(reason) when is_atom(reason), do: Atom.to_string(reason)
  defp to_reason(reason) when is_binary(reason), do: reason
  defp to_reason(reason), do: inspect(reason)

  defp message(%Sykli.Error{message: message}, _default) when is_binary(message), do: message
  defp message(reason, default) when reason in [nil, ""], do: default
  defp message(reason, _default) when is_binary(reason), do: reason
  defp message(reason, _default), do: inspect(reason)

  defp maybe_put(map, _key, nil), do: map
  defp maybe_put(map, _key, value) when value == %{}, do: map
  defp maybe_put(map, key, value), do: Map.put(map, key, value)

  defp non_empty_map(map) when map == %{}, do: nil
  defp non_empty_map(map), do: map

  defp atom_to_string(nil), do: nil
  defp atom_to_string(atom), do: Atom.to_string(atom)

  defp parse_atom(value, allowed, default) when is_binary(value) do
    atom = String.to_existing_atom(value)
    if atom in allowed, do: atom, else: default
  rescue
    ArgumentError -> default
  end

  defp parse_atom(_value, _allowed, default), do: default

  defp string_or_default(value, _default) when is_binary(value), do: value
  defp string_or_default(_value, default), do: default

  defp stringify_keys(map) when is_map(map) do
    Map.new(map, fn
      {key, value} when is_atom(key) -> {Atom.to_string(key), stringify_keys(value)}
      {key, value} -> {key, stringify_keys(value)}
    end)
  end

  defp stringify_keys(list) when is_list(list), do: Enum.map(list, &stringify_keys/1)
  defp stringify_keys(value), do: value
end
