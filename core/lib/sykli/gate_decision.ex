defmodule Sykli.GateDecision do
  @moduledoc """
  Local gate decision model for Team Mode.

  A gate decision is a local coordination record stored under
  `.sykli/gates/<id>.json`. It records a waiting or blocked decision boundary and
  the later approval or rejection. Coordinator sync and runtime gate hookup are
  later Team Mode layers.
  """

  @version "1"
  @statuses ~w(waiting approved rejected blocked expired)
  @requester_types ~w(member agent daemon system)
  @terminal_statuses ~w(approved rejected expired)

  @enforce_keys [:id, :status, :created_at, :updated_at]
  defstruct [
    :id,
    version: @version,
    work_item_id: nil,
    run_id: nil,
    node_id: nil,
    status: "waiting",
    reason: nil,
    requested_by_type: nil,
    requested_by_id: nil,
    decided_by: nil,
    decided_at: nil,
    created_at: nil,
    updated_at: nil,
    evidence_refs: []
  ]

  @type status :: String.t()
  @type t :: %__MODULE__{
          id: String.t(),
          version: String.t(),
          work_item_id: String.t() | nil,
          run_id: String.t() | nil,
          node_id: String.t() | nil,
          status: status(),
          reason: String.t() | nil,
          requested_by_type: String.t() | nil,
          requested_by_id: String.t() | nil,
          decided_by: String.t() | nil,
          decided_at: String.t() | nil,
          created_at: String.t(),
          updated_at: String.t(),
          evidence_refs: [map()]
        }

  def version, do: @version
  def statuses, do: @statuses
  def requester_types, do: @requester_types

  @doc "Builds a new local gate request."
  def new(opts \\ []) do
    now = Keyword.get_lazy(opts, :now, &now_iso8601/0)

    attrs = %{
      id: Keyword.get_lazy(opts, :id, &Sykli.ULID.generate/0),
      version: @version,
      work_item_id: blank_to_nil(Keyword.get(opts, :work_item_id)),
      run_id: blank_to_nil(Keyword.get(opts, :run_id)),
      node_id: blank_to_nil(Keyword.get(opts, :node_id)),
      status: Keyword.get(opts, :status, "waiting"),
      reason: blank_to_nil(Keyword.get(opts, :reason)),
      requested_by_type: Keyword.get(opts, :requested_by_type),
      requested_by_id: blank_to_nil(Keyword.get(opts, :requested_by_id)),
      decided_by: blank_to_nil(Keyword.get(opts, :decided_by)),
      decided_at: blank_to_nil(Keyword.get(opts, :decided_at)),
      created_at: now,
      updated_at: now,
      evidence_refs: Keyword.get(opts, :evidence_refs, [])
    }

    from_map(attrs)
  end

  @doc "Approves a waiting or blocked gate."
  def approve(%__MODULE__{} = gate, reason, opts \\ []) do
    decide(gate, "approved", reason, opts)
  end

  @doc "Rejects a waiting or blocked gate."
  def reject(%__MODULE__{} = gate, reason, opts \\ []) do
    decide(gate, "rejected", reason, opts)
  end

  @doc "Converts a gate decision to the persisted JSON map shape."
  def to_map(%__MODULE__{} = gate) do
    %{
      "id" => gate.id,
      "version" => gate.version,
      "work_item_id" => gate.work_item_id,
      "run_id" => gate.run_id,
      "node_id" => gate.node_id,
      "status" => gate.status,
      "reason" => gate.reason,
      "requested_by_type" => gate.requested_by_type,
      "requested_by_id" => gate.requested_by_id,
      "decided_by" => gate.decided_by,
      "decided_at" => gate.decided_at,
      "created_at" => gate.created_at,
      "updated_at" => gate.updated_at,
      "evidence_refs" => gate.evidence_refs
    }
  end

  @doc "Builds a gate decision from a persisted JSON map."
  def from_map(map) when is_map(map) do
    attrs = normalize_keys(map)

    with :ok <- validate_id(attrs["id"]),
         :ok <- validate_version(attrs["version"]),
         :ok <- validate_status(attrs["status"] || "waiting"),
         :ok <- validate_optional_id(attrs["work_item_id"], :work_item_id),
         :ok <- validate_optional_string(attrs["run_id"], :run_id),
         :ok <- validate_optional_string(attrs["node_id"], :node_id),
         :ok <- validate_requested_by(attrs["requested_by_type"], attrs["requested_by_id"]),
         :ok <- validate_optional_string(attrs["decided_by"], :decided_by),
         :ok <- validate_optional_string(attrs["decided_at"], :decided_at),
         :ok <- validate_evidence_refs(attrs["evidence_refs"] || []) do
      {:ok,
       %__MODULE__{
         id: attrs["id"],
         version: attrs["version"],
         work_item_id: blank_to_nil(attrs["work_item_id"]),
         run_id: blank_to_nil(attrs["run_id"]),
         node_id: blank_to_nil(attrs["node_id"]),
         status: attrs["status"] || "waiting",
         reason: blank_to_nil(attrs["reason"]),
         requested_by_type: attrs["requested_by_type"],
         requested_by_id: blank_to_nil(attrs["requested_by_id"]),
         decided_by: blank_to_nil(attrs["decided_by"]),
         decided_at: blank_to_nil(attrs["decided_at"]),
         created_at: attrs["created_at"] || now_iso8601(),
         updated_at: attrs["updated_at"] || attrs["created_at"] || now_iso8601(),
         evidence_refs: Enum.map(attrs["evidence_refs"] || [], &normalize_keys/1)
       }}
    end
  end

  def from_map(_), do: {:error, {:invalid_gate_decision, :not_object}}

  def validate_id(id) when is_binary(id) do
    if Regex.match?(~r/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/, id) do
      :ok
    else
      {:error, {:invalid_gate_id, id}}
    end
  end

  def validate_id(id), do: {:error, {:invalid_gate_id, id}}

  def validate_status(status) when status in @statuses, do: :ok
  def validate_status(status), do: {:error, {:invalid_gate_status, status}}

  defp decide(%__MODULE__{} = gate, target_status, reason, opts) do
    with :ok <- validate_transition(gate.status, target_status),
         :ok <- validate_decision_reason(reason) do
      now = Keyword.get_lazy(opts, :now, &now_iso8601/0)

      {:ok,
       %__MODULE__{
         gate
         | status: target_status,
           reason: String.trim(reason),
           decided_by: blank_to_nil(Keyword.get(opts, :decided_by)),
           decided_at: now,
           updated_at: now
       }}
    end
  end

  defp validate_transition(from, to)
       when from in ["waiting", "blocked"] and to in ["approved", "rejected"],
       do: :ok

  defp validate_transition(from, to) when from in @terminal_statuses,
    do: {:error, {:invalid_gate_transition, from, to}}

  defp validate_transition(from, to), do: {:error, {:invalid_gate_transition, from, to}}

  defp validate_decision_reason(reason) when is_binary(reason) do
    if String.trim(reason) == "", do: {:error, :gate_decision_missing_reason}, else: :ok
  end

  defp validate_decision_reason(_), do: {:error, :gate_decision_missing_reason}

  defp validate_version(nil), do: {:error, {:missing_gate_version, nil}}
  defp validate_version(@version), do: :ok
  defp validate_version(version), do: {:error, {:unsupported_gate_version, version}}

  defp validate_optional_id(nil, _field), do: :ok
  defp validate_optional_id("", _field), do: :ok
  defp validate_optional_id(id, _field), do: Sykli.WorkItem.validate_id(id)

  defp validate_optional_string(nil, _field), do: :ok
  defp validate_optional_string("", _field), do: :ok

  defp validate_optional_string(value, field) when is_binary(value) do
    if String.trim(value) == "", do: {:error, {:invalid_gate_field, field, :empty}}, else: :ok
  end

  defp validate_optional_string(value, field), do: {:error, {:invalid_gate_field, field, value}}

  defp validate_requested_by(nil, nil), do: :ok
  defp validate_requested_by(nil, requester_id) when requester_id in [nil, ""], do: :ok

  defp validate_requested_by(nil, _requester_id),
    do: {:error, {:invalid_gate_requester, :missing_type}}

  defp validate_requested_by(type, requester_id) when type in @requester_types do
    validate_requester_id(requester_id)
  end

  defp validate_requested_by(type, _requester_id),
    do: {:error, {:invalid_gate_requester_type, type}}

  defp validate_requester_id(nil), do: {:error, {:invalid_gate_requester_id, :empty}}

  defp validate_requester_id(id) when is_binary(id) do
    if String.trim(id) == "", do: {:error, {:invalid_gate_requester_id, :empty}}, else: :ok
  end

  defp validate_requester_id(id), do: {:error, {:invalid_gate_requester_id, id}}

  defp validate_evidence_refs(refs) when is_list(refs) do
    Enum.reduce_while(refs, :ok, fn
      ref, :ok when is_map(ref) -> {:cont, :ok}
      ref, :ok -> {:halt, {:error, {:invalid_gate_evidence_ref, ref}}}
    end)
  end

  defp validate_evidence_refs(refs), do: {:error, {:invalid_gate_evidence_refs, refs}}

  defp normalize_keys(map) do
    Map.new(map, fn
      {key, value} when is_atom(key) -> {Atom.to_string(key), value}
      {key, value} -> {key, value}
    end)
  end

  defp blank_to_nil(value) when is_binary(value) do
    value = String.trim(value)
    if value == "", do: nil, else: value
  end

  defp blank_to_nil(value), do: value

  defp now_iso8601 do
    DateTime.utc_now()
    |> DateTime.truncate(:second)
    |> DateTime.to_iso8601()
  end
end
