defmodule Sykli.TeamCoordinator.GateDecisionSummary do
  @moduledoc """
  Metadata-only Team Mode projection of a local gate decision.

  The summary intentionally carries only the gate metadata needed for team
  coordination.
  """

  alias Sykli.GateDecision

  @fields ~w(id run_id work_item_id status decided_by decided_at reason)

  @enforce_keys [:gate]
  defstruct gate: %{}

  def from_gate_decision(%GateDecision{} = gate) do
    %__MODULE__{
      gate: %{
        "id" => gate.id,
        "run_id" => gate.run_id,
        "work_item_id" => gate.work_item_id,
        "status" => gate.status,
        "decided_by" => gate.decided_by,
        "decided_at" => gate.decided_at,
        "reason" => gate.reason
      }
    }
  end

  def from_map(map) when is_map(map) do
    with :ok <- validate_keys(map),
         {:ok, id} <- required_string(map, "id"),
         :ok <- GateDecision.validate_id(id),
         {:ok, run_id} <- required_string(map, "run_id"),
         :ok <- validate_optional_string(map["work_item_id"]),
         :ok <- GateDecision.validate_status(map["status"]),
         :ok <- validate_optional_string(map["decided_by"]),
         :ok <- validate_optional_string(map["decided_at"]),
         :ok <- validate_optional_string(map["reason"]) do
      {:ok,
       %__MODULE__{gate: Map.take(map, @fields) |> Map.put("id", id) |> Map.put("run_id", run_id)}}
    else
      {:error, {:invalid_gate_id, _id}} = error -> error
      {:error, {:invalid_gate_status, _status}} = error -> error
      {:error, _reason} -> {:error, :team_gate_invalid_payload}
    end
  end

  def from_map(_), do: {:error, :team_gate_invalid_payload}

  def encode(%__MODULE__{gate: gate}), do: gate

  defp validate_keys(map) do
    extra = Map.keys(map) -- @fields
    if extra == [], do: :ok, else: {:error, {:unexpected_fields, extra}}
  end

  defp required_string(map, field) do
    case Map.get(map, field) do
      value when is_binary(value) ->
        value = String.trim(value)
        if value == "", do: {:error, {:missing_field, field}}, else: {:ok, value}

      _ ->
        {:error, {:missing_field, field}}
    end
  end

  defp validate_optional_string(nil), do: :ok
  defp validate_optional_string(value) when is_binary(value), do: :ok
  defp validate_optional_string(_value), do: {:error, :invalid_optional_string}
end
