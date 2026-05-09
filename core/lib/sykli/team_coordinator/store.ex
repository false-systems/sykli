defmodule Sykli.TeamCoordinator.Store do
  @moduledoc """
  In-memory store for the self-hosted Team Mode coordinator skeleton.

  This store exists to make the Phase 4 API testable without adding database
  migrations in the same PR. It is not production persistence; Postgres remains
  the intended durable coordinator store in the next implementation layer.
  """

  use GenServer

  alias Sykli.WorkItem

  @assignment_types WorkItem.assignment_types()

  def start_link(opts \\ []) do
    {server_opts, init_opts} = Keyword.split(opts, [:name])
    GenServer.start_link(__MODULE__, init_opts, server_opts)
  end

  def create_org(server, attrs), do: GenServer.call(server, {:create_org, attrs})
  def list_orgs(server), do: GenServer.call(server, :list_orgs)
  def create_team(server, attrs), do: GenServer.call(server, {:create_team, attrs})
  def list_teams(server), do: GenServer.call(server, :list_teams)
  def create_work_item(server, attrs), do: GenServer.call(server, {:create_work_item, attrs})

  def list_work_items(server, filters \\ %{}),
    do: GenServer.call(server, {:list_work_items, filters})

  def get_work_item(server, id), do: GenServer.call(server, {:get_work_item, id})

  def claim_work_item(server, id, attrs),
    do: GenServer.call(server, {:claim_work_item, id, attrs})

  def add_note(server, id, attrs), do: GenServer.call(server, {:add_note, id, attrs})
  def audit_log(server), do: GenServer.call(server, :audit_log)

  @impl true
  def init(opts) do
    {:ok,
     %{
       now: Keyword.get(opts, :now, &DateTime.utc_now/0),
       id: Keyword.get(opts, :id, &Sykli.ULID.generate/0),
       orgs: %{},
       orgs_by_slug: %{},
       teams: %{},
       teams_by_org_slug: %{},
       work_items: %{},
       notes_by_work_item: %{},
       audit_log: []
     }}
  end

  @impl true
  def handle_call({:create_org, attrs}, _from, state) do
    with {:ok, slug} <- required_slug(attrs, "slug"),
         {:ok, name} <- required_string(attrs, "name"),
         :ok <- ensure_absent(state.orgs_by_slug, slug, {:duplicate_org_slug, slug}) do
      org = %{
        "id" => id(state),
        "slug" => slug,
        "name" => name,
        "created_at" => now(state)
      }

      state =
        state
        |> put_in([:orgs, org["id"]], org)
        |> put_in([:orgs_by_slug, slug], org["id"])
        |> audit("org.created", "org", org["id"], org["id"], nil)

      {:reply, {:ok, org}, state}
    else
      {:error, reason} -> {:reply, {:error, reason}, state}
    end
  end

  def handle_call(:list_orgs, _from, state) do
    {:reply, {:ok, sorted_values(state.orgs)}, state}
  end

  def handle_call({:create_team, attrs}, _from, state) do
    with {:ok, org_id} <- org_id(state, attrs),
         {:ok, slug} <- required_slug(attrs, "slug"),
         {:ok, name} <- required_string(attrs, "name"),
         :ok <-
           ensure_absent(state.teams_by_org_slug, {org_id, slug}, {:duplicate_team_slug, slug}) do
      team = %{
        "id" => id(state),
        "org_id" => org_id,
        "slug" => slug,
        "name" => name,
        "created_at" => now(state)
      }

      state =
        state
        |> put_in([:teams, team["id"]], team)
        |> put_in([:teams_by_org_slug, {org_id, slug}], team["id"])
        |> audit("team.created", "team", team["id"], org_id, team["id"])

      {:reply, {:ok, team}, state}
    else
      {:error, reason} -> {:reply, {:error, reason}, state}
    end
  end

  def handle_call(:list_teams, _from, state) do
    {:reply, {:ok, sorted_values(state.teams)}, state}
  end

  def handle_call({:create_work_item, attrs}, _from, state) do
    with {:ok, org_id} <- org_id(state, attrs),
         {:ok, team_id} <- team_id(state, attrs, org_id),
         {:ok, title} <- required_string(attrs, "title") do
      now = now(state)

      item = %{
        "id" => id(state),
        "org_id" => org_id,
        "team_id" => team_id,
        "title" => title,
        "intent" => blank_to_nil(Map.get(attrs, "intent")),
        "status" => "open",
        "created_by" => blank_to_nil(Map.get(attrs, "created_by")),
        "assigned_to_type" => nil,
        "assigned_to_id" => nil,
        "created_at" => now,
        "updated_at" => now
      }

      state =
        state
        |> put_in([:work_items, item["id"]], item)
        |> put_in([:notes_by_work_item, item["id"]], [])
        |> audit("work.created", "work_item", item["id"], org_id, team_id)

      {:reply, {:ok, item}, state}
    else
      {:error, reason} -> {:reply, {:error, reason}, state}
    end
  end

  def handle_call({:list_work_items, filters}, _from, state) do
    items =
      state.work_items
      |> sorted_values()
      |> Enum.filter(fn item ->
        filter_match?(item, "org_id", Map.get(filters, "org_id")) and
          filter_match?(item, "team_id", Map.get(filters, "team_id"))
      end)

    {:reply, {:ok, items}, state}
  end

  def handle_call({:get_work_item, id}, _from, state) do
    {:reply, fetch_work_item(state, id), state}
  end

  def handle_call({:claim_work_item, id, attrs}, _from, state) do
    with {:ok, item} <- fetch_work_item(state, id),
         :ok <- ensure_claimable(item),
         {:ok, assignment_type} <- required_assignment_type(attrs),
         {:ok, assignment_id} <- required_string(attrs, "assigned_to_id") do
      now = now(state)

      updated =
        item
        |> Map.put("status", "claimed")
        |> Map.put("assigned_to_type", assignment_type)
        |> Map.put("assigned_to_id", assignment_id)
        |> Map.put("updated_at", now)

      state =
        state
        |> put_in([:work_items, id], updated)
        |> audit("work.claimed", "work_item", id, item["org_id"], item["team_id"])

      {:reply, {:ok, updated}, state}
    else
      {:error, reason} -> {:reply, {:error, reason}, state}
    end
  end

  def handle_call({:add_note, id, attrs}, _from, state) do
    with {:ok, item} <- fetch_work_item(state, id),
         {:ok, body} <- required_string(attrs, "body") do
      note = %{
        "id" => id(state),
        "work_item_id" => id,
        "author_type" => blank_to_nil(Map.get(attrs, "author_type")),
        "author_id" => blank_to_nil(Map.get(attrs, "author_id")),
        "body" => body,
        "created_at" => now(state)
      }

      notes = Map.get(state.notes_by_work_item, id, []) ++ [note]
      updated = Map.put(item, "updated_at", note["created_at"])

      state =
        state
        |> put_in([:notes_by_work_item, id], notes)
        |> put_in([:work_items, id], updated)
        |> audit("work.note_added", "work_item", id, item["org_id"], item["team_id"])

      {:reply, {:ok, note}, state}
    else
      {:error, reason} -> {:reply, {:error, reason}, state}
    end
  end

  def handle_call(:audit_log, _from, state) do
    {:reply, {:ok, Enum.reverse(state.audit_log)}, state}
  end

  defp required_slug(attrs, field) do
    with {:ok, value} <- required_string(attrs, field) do
      if Regex.match?(~r/^[a-z0-9][a-z0-9-]{0,62}$/, value) do
        {:ok, value}
      else
        {:error, {:invalid_field, field}}
      end
    end
  end

  defp required_string(attrs, field) do
    case Map.get(attrs, field) do
      value when is_binary(value) ->
        trimmed = String.trim(value)
        if trimmed == "", do: {:error, {:missing_field, field}}, else: {:ok, trimmed}

      _ ->
        {:error, {:missing_field, field}}
    end
  end

  defp org_id(state, attrs) do
    cond do
      is_binary(Map.get(attrs, "org_id")) and Map.has_key?(state.orgs, Map.get(attrs, "org_id")) ->
        {:ok, Map.get(attrs, "org_id")}

      is_binary(Map.get(attrs, "org_slug")) ->
        case Map.fetch(state.orgs_by_slug, Map.get(attrs, "org_slug")) do
          {:ok, id} -> {:ok, id}
          :error -> {:error, {:org_not_found, Map.get(attrs, "org_slug")}}
        end

      true ->
        {:error, {:org_not_found, Map.get(attrs, "org_id") || Map.get(attrs, "org_slug")}}
    end
  end

  defp team_id(state, attrs, org_id) do
    cond do
      is_binary(Map.get(attrs, "team_id")) and
          match?(%{"org_id" => ^org_id}, Map.get(state.teams, Map.get(attrs, "team_id"))) ->
        {:ok, Map.get(attrs, "team_id")}

      is_binary(Map.get(attrs, "team_slug")) ->
        case Map.fetch(state.teams_by_org_slug, {org_id, Map.get(attrs, "team_slug")}) do
          {:ok, id} -> {:ok, id}
          :error -> {:error, {:team_not_found, Map.get(attrs, "team_slug")}}
        end

      true ->
        {:error, {:team_not_found, Map.get(attrs, "team_id") || Map.get(attrs, "team_slug")}}
    end
  end

  defp required_assignment_type(attrs) do
    case Map.get(attrs, "assigned_to_type") || Map.get(attrs, "assignee_type") do
      type when type in @assignment_types -> {:ok, type}
      type -> {:error, {:invalid_assignment_type, type}}
    end
  end

  defp fetch_work_item(state, id) do
    with :ok <- WorkItem.validate_id(id) do
      case Map.fetch(state.work_items, id) do
        {:ok, item} -> {:ok, item}
        :error -> {:error, {:work_item_not_found, id}}
      end
    end
  end

  defp ensure_claimable(%{"status" => "open"}), do: :ok

  defp ensure_claimable(item) do
    {:error,
     {:work_item_already_claimed, item["id"],
      %{
        "status" => item["status"],
        "assigned_to_type" => item["assigned_to_type"],
        "assigned_to_id" => item["assigned_to_id"]
      }}}
  end

  defp ensure_absent(map, key, reason) do
    if Map.has_key?(map, key), do: {:error, reason}, else: :ok
  end

  defp sorted_values(map) do
    map
    |> Map.values()
    |> Enum.sort_by(& &1["id"])
  end

  defp filter_match?(_item, _field, nil), do: true
  defp filter_match?(item, field, value), do: item[field] == value

  defp audit(state, action, subject_type, subject_id, org_id, team_id) do
    event = %{
      "id" => id(state),
      "org_id" => org_id,
      "team_id" => team_id,
      "actor_type" => "system",
      "actor_id" => "coordinator",
      "action" => action,
      "subject_type" => subject_type,
      "subject_id" => subject_id,
      "metadata" => %{},
      "created_at" => now(state)
    }

    update_in(state.audit_log, &[event | &1])
  end

  defp id(state), do: state.id.()

  defp now(state) do
    state.now.()
    |> case do
      %DateTime{} = dt -> DateTime.to_iso8601(dt)
      value when is_binary(value) -> value
    end
  end

  defp blank_to_nil(value) when is_binary(value) do
    value = String.trim(value)
    if value == "", do: nil, else: value
  end

  defp blank_to_nil(_), do: nil
end
