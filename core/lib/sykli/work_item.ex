defmodule Sykli.WorkItem do
  @moduledoc """
  Local work item model for Team Mode.

  A work item is a local coordination record stored under
  `.sykli/work/items/<id>.json`. Networking and coordinator sync are added in
  later Team Mode phases; this module only defines the local shape and
  validation rules.
  """

  @version "1"
  @statuses ~w(open claimed running blocked done failed cancelled)
  @actor_types ~w(member agent daemon)
  @assignment_types @actor_types

  @enforce_keys [:id, :title, :status, :created_at, :updated_at]
  defstruct [
    :id,
    version: @version,
    title: nil,
    intent: nil,
    status: "open",
    created_by_type: nil,
    created_by_id: nil,
    assigned_to_type: nil,
    assigned_to_id: nil,
    created_at: nil,
    updated_at: nil,
    notes: []
  ]

  @type status :: String.t()
  @type assignment_type :: String.t() | nil

  @type note :: %{
          required(String.t()) => String.t() | nil
        }

  @type t :: %__MODULE__{
          id: String.t(),
          version: String.t(),
          title: String.t(),
          intent: String.t() | nil,
          status: status(),
          created_by_type: assignment_type(),
          created_by_id: String.t() | nil,
          assigned_to_type: assignment_type(),
          assigned_to_id: String.t() | nil,
          created_at: String.t(),
          updated_at: String.t(),
          notes: [map()]
        }

  @doc "Returns the current persisted work item schema version."
  def version, do: @version

  @doc "Returns valid work item status strings."
  def statuses, do: @statuses

  @doc "Returns valid assignment type strings."
  def assignment_types, do: @assignment_types

  @doc "Returns valid work item actor type strings."
  def actor_types, do: @actor_types

  @doc "Builds a new work item."
  def new(title, opts \\ [])

  def new(title, opts) when is_binary(title) do
    now = Keyword.get_lazy(opts, :now, &now_iso8601/0)

    attrs = %{
      id: Keyword.get_lazy(opts, :id, &Sykli.ULID.generate/0),
      version: @version,
      title: String.trim(title),
      intent: blank_to_nil(Keyword.get(opts, :intent)),
      status: Keyword.get(opts, :status, "open"),
      created_by_type: Keyword.get(opts, :created_by_type),
      created_by_id: blank_to_nil(Keyword.get(opts, :created_by_id)),
      assigned_to_type: Keyword.get(opts, :assigned_to_type),
      assigned_to_id: blank_to_nil(Keyword.get(opts, :assigned_to_id)),
      created_at: now,
      updated_at: now,
      notes: Keyword.get(opts, :notes, [])
    }

    from_map(attrs)
  end

  def new(_title, _opts), do: {:error, {:invalid_title, :not_string}}

  @doc "Updates a work item's status."
  def update_status(%__MODULE__{} = item, status, opts \\ []) do
    with :ok <- validate_status(status) do
      {:ok,
       %__MODULE__{
         item
         | status: status,
           updated_at: Keyword.get_lazy(opts, :now, &now_iso8601/0)
       }}
    end
  end

  @doc "Claims a work item for a member, agent, or daemon."
  def claim(%__MODULE__{} = item, assignment_type, assignment_id, opts \\ []) do
    with :ok <- ensure_claimable(item),
         :ok <- validate_assignment_type(assignment_type),
         :ok <- validate_assignment_id(assignment_id) do
      now = Keyword.get_lazy(opts, :now, &now_iso8601/0)

      {:ok,
       %__MODULE__{
         item
         | status: "claimed",
           assigned_to_type: assignment_type,
           assigned_to_id: assignment_id,
           updated_at: now
       }}
    end
  end

  @doc "Appends a note to a work item."
  def append_note(item, body, opts \\ [])

  def append_note(%__MODULE__{} = item, body, opts) when is_binary(body) do
    trimmed = String.trim(body)

    if trimmed == "" do
      {:error, {:invalid_note, :empty_body}}
    else
      now = Keyword.get_lazy(opts, :now, &now_iso8601/0)

      note = %{
        "id" => Keyword.get_lazy(opts, :note_id, &Sykli.ULID.generate/0),
        "author_type" => blank_to_nil(Keyword.get(opts, :author_type)),
        "author_id" => blank_to_nil(Keyword.get(opts, :author_id)),
        "body" => trimmed,
        "created_at" => now
      }

      {:ok, %__MODULE__{item | notes: item.notes ++ [note], updated_at: now}}
    end
  end

  def append_note(%__MODULE__{}, _body, _opts), do: {:error, {:invalid_note, :not_string}}

  @doc "Converts a work item to the persisted JSON map shape."
  def to_map(%__MODULE__{} = item) do
    %{
      "id" => item.id,
      "version" => item.version,
      "title" => item.title,
      "intent" => item.intent,
      "status" => item.status,
      "created_by_type" => item.created_by_type,
      "created_by_id" => item.created_by_id,
      "assigned_to_type" => item.assigned_to_type,
      "assigned_to_id" => item.assigned_to_id,
      "created_at" => item.created_at,
      "updated_at" => item.updated_at,
      "notes" => item.notes
    }
  end

  @doc "Builds a work item from a persisted JSON map."
  def from_map(map) when is_map(map) do
    attrs = normalize_keys(map)

    with :ok <- validate_id(attrs["id"]),
         :ok <- validate_title(attrs["title"]),
         :ok <- validate_version(attrs["version"]),
         :ok <- validate_status(attrs["status"] || "open"),
         :ok <- validate_created_by(attrs["created_by_type"], attrs["created_by_id"]),
         :ok <- validate_assignment_type(attrs["assigned_to_type"]),
         :ok <-
           validate_optional_assignment_id(attrs["assigned_to_type"], attrs["assigned_to_id"]),
         :ok <- validate_notes(attrs["notes"] || []) do
      notes = Enum.map(attrs["notes"] || [], &normalize_keys/1)

      {:ok,
       %__MODULE__{
         id: attrs["id"],
         version: attrs["version"],
         title: String.trim(attrs["title"]),
         intent: blank_to_nil(attrs["intent"]),
         status: attrs["status"] || "open",
         created_by_type: attrs["created_by_type"],
         created_by_id: blank_to_nil(attrs["created_by_id"]),
         assigned_to_type: attrs["assigned_to_type"],
         assigned_to_id: blank_to_nil(attrs["assigned_to_id"]),
         created_at: attrs["created_at"] || now_iso8601(),
         updated_at: attrs["updated_at"] || attrs["created_at"] || now_iso8601(),
         notes: notes
       }}
    end
  end

  def from_map(_), do: {:error, {:invalid_work_item, :not_object}}

  def validate_id(id) when is_binary(id) do
    if Regex.match?(~r/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/, id) do
      :ok
    else
      {:error, {:invalid_work_item_id, id}}
    end
  end

  def validate_id(id), do: {:error, {:invalid_work_item_id, id}}

  def validate_status(status) when status in @statuses, do: :ok
  def validate_status(status), do: {:error, {:invalid_work_item_status, status}}

  def validate_assignment_type(nil), do: :ok
  def validate_assignment_type(type) when type in @assignment_types, do: :ok
  def validate_assignment_type(type), do: {:error, {:invalid_assignment_type, type}}

  def validate_actor_type(nil), do: :ok
  def validate_actor_type(type) when type in @actor_types, do: :ok
  def validate_actor_type(type), do: {:error, {:invalid_actor_type, type}}

  defp validate_title(title) when is_binary(title) do
    if String.trim(title) == "", do: {:error, {:invalid_title, :empty}}, else: :ok
  end

  defp validate_title(title), do: {:error, {:invalid_title, title}}

  defp validate_version(nil), do: {:error, {:missing_work_item_version, nil}}
  defp validate_version(@version), do: :ok
  defp validate_version(version), do: {:error, {:unsupported_work_item_version, version}}

  defp ensure_claimable(%__MODULE__{status: "open"}), do: :ok

  defp ensure_claimable(%__MODULE__{} = item) do
    {:error,
     {:work_item_already_claimed, item.id,
      %{
        "status" => item.status,
        "assigned_to_type" => item.assigned_to_type,
        "assigned_to_id" => item.assigned_to_id
      }}}
  end

  defp validate_created_by(nil, nil), do: :ok
  defp validate_created_by(nil, actor_id) when actor_id in [nil, ""], do: :ok
  defp validate_created_by(nil, _actor_id), do: {:error, {:invalid_created_by, :missing_type}}

  defp validate_created_by(actor_type, actor_id) do
    with :ok <- validate_actor_type(actor_type),
         :ok <- validate_actor_id(actor_id, :created_by) do
      :ok
    end
  end

  defp validate_optional_assignment_id(nil, nil), do: :ok

  defp validate_optional_assignment_id(nil, assignment_id) when assignment_id in [nil, ""],
    do: :ok

  defp validate_optional_assignment_id(nil, _assignment_id) do
    {:error, {:invalid_assignment_id, :missing_type}}
  end

  defp validate_optional_assignment_id(_assignment_type, assignment_id) do
    validate_assignment_id(assignment_id)
  end

  defp validate_assignment_id(nil), do: {:error, {:invalid_assignment_id, :empty}}

  defp validate_assignment_id(id) when is_binary(id) do
    if String.trim(id) == "", do: {:error, {:invalid_assignment_id, :empty}}, else: :ok
  end

  defp validate_assignment_id(id), do: {:error, {:invalid_assignment_id, id}}

  defp validate_actor_id(nil, field), do: {:error, {:invalid_actor_id, field, :empty}}

  defp validate_actor_id(id, field) when is_binary(id) do
    if String.trim(id) == "", do: {:error, {:invalid_actor_id, field, :empty}}, else: :ok
  end

  defp validate_actor_id(id, field), do: {:error, {:invalid_actor_id, field, id}}

  defp validate_notes(notes) when is_list(notes) do
    Enum.reduce_while(notes, :ok, fn note, :ok ->
      case validate_note(note) do
        :ok -> {:cont, :ok}
        {:error, reason} -> {:halt, {:error, reason}}
      end
    end)
  end

  defp validate_notes(notes), do: {:error, {:invalid_notes, notes}}

  defp validate_note(note) when is_map(note) do
    attrs = normalize_keys(note)

    with :ok <- validate_note_string(attrs["id"], :id),
         :ok <- validate_note_string(attrs["body"], :body),
         :ok <- validate_note_string(attrs["created_at"], :created_at),
         :ok <- validate_note_author(attrs["author_type"], attrs["author_id"]) do
      :ok
    end
  end

  defp validate_note(note), do: {:error, {:invalid_note, note}}

  defp validate_note_string(value, field) when is_binary(value) do
    if String.trim(value) == "", do: {:error, {:invalid_note, {field, :empty}}}, else: :ok
  end

  defp validate_note_string(value, field), do: {:error, {:invalid_note, {field, value}}}

  defp validate_note_author(nil, nil), do: :ok
  defp validate_note_author(nil, author_id) when author_id in [nil, ""], do: :ok
  defp validate_note_author(nil, _author_id), do: {:error, {:invalid_note_author, :missing_type}}

  defp validate_note_author(author_type, author_id) do
    with :ok <- validate_actor_type(author_type),
         :ok <- validate_actor_id(author_id, :note_author) do
      :ok
    end
  end

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
