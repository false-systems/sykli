defmodule Sykli.Coordinator.StoreTest do
  use ExUnit.Case, async: true

  alias Sykli.Coordinator.Store

  @now "2026-05-09T10:00:00Z"

  setup do
    {:ok, store} =
      Store.start_link(
        now: fn -> @now end,
        id:
          id_sequence(
            ~w(org_001 audit_001 team_001 audit_002 work_001 audit_003 audit_004 note_001 audit_005)
          )
      )

    {:ok, store: store}
  end

  test "creates orgs and rejects duplicate slugs", %{store: store} do
    assert {:ok, org} =
             Store.create_org(store, %{"slug" => "false-systems", "name" => "False Systems"})

    assert org["id"] == "org_001"
    assert org["created_at"] == @now

    assert {:error, {:duplicate_org_slug, "false-systems"}} =
             Store.create_org(store, %{"slug" => "false-systems", "name" => "Duplicate"})
  end

  test "creates teams under orgs and rejects missing or duplicate teams", %{store: store} do
    assert {:error, {:org_not_found, "missing"}} =
             Store.create_team(store, %{
               "org_slug" => "missing",
               "slug" => "platform",
               "name" => "Platform"
             })

    assert {:ok, org} =
             Store.create_org(store, %{"slug" => "false-systems", "name" => "False Systems"})

    assert {:ok, team} =
             Store.create_team(store, %{
               "org_id" => org["id"],
               "slug" => "platform",
               "name" => "Platform"
             })

    assert team["id"] == "team_001"
    assert team["org_id"] == org["id"]

    assert {:error, {:duplicate_team_slug, "platform"}} =
             Store.create_team(store, %{
               "org_id" => org["id"],
               "slug" => "platform",
               "name" => "Again"
             })
  end

  test "creates lists shows claims and notes work items", %{store: store} do
    {:ok, org} = Store.create_org(store, %{"slug" => "false-systems", "name" => "False Systems"})

    {:ok, team} =
      Store.create_team(store, %{
        "org_id" => org["id"],
        "slug" => "platform",
        "name" => "Platform"
      })

    assert {:ok, item} =
             Store.create_work_item(store, %{
               "org_id" => org["id"],
               "team_id" => team["id"],
               "title" => "Investigate deploy",
               "intent" => "Find the failing step"
             })

    assert item["id"] == "work_001"
    assert item["status"] == "open"
    refute Map.has_key?(item, "logs")
    refute Map.has_key?(item, "artifacts")
    refute Map.has_key?(item, "secrets")

    assert {:ok, [^item]} = Store.list_work_items(store)
    assert {:ok, ^item} = Store.get_work_item(store, item["id"])

    assert {:ok, claimed} =
             Store.claim_work_item(store, item["id"], %{
               "assigned_to_type" => "member",
               "assigned_to_id" => "yair"
             })

    assert claimed["status"] == "claimed"
    assert claimed["assigned_to_type"] == "member"
    assert claimed["assigned_to_id"] == "yair"

    assert {:error, {:work_item_already_claimed, "work_001", _assignment}} =
             Store.claim_work_item(store, item["id"], %{
               "assigned_to_type" => "agent",
               "assigned_to_id" => "claude"
             })

    assert {:ok, note} =
             Store.add_note(store, item["id"], %{
               "author_type" => "member",
               "author_id" => "yair",
               "body" => "Found likely API breakage"
             })

    assert note["work_item_id"] == item["id"]
    assert note["body"] == "Found likely API breakage"
  end

  test "validates work payloads", %{store: store} do
    assert {:error, {:org_not_found, nil}} =
             Store.create_work_item(store, %{"title" => "No team"})

    {:ok, org} = Store.create_org(store, %{"slug" => "false-systems", "name" => "False Systems"})

    {:ok, team} =
      Store.create_team(store, %{
        "org_id" => org["id"],
        "slug" => "platform",
        "name" => "Platform"
      })

    {:ok, item} =
      Store.create_work_item(store, %{
        "org_id" => org["id"],
        "team_id" => team["id"],
        "title" => "Task"
      })

    assert {:error, {:invalid_assignment_type, "robot"}} =
             Store.claim_work_item(store, item["id"], %{
               "assigned_to_type" => "robot",
               "assigned_to_id" => "r2d2"
             })

    assert {:error, {:missing_field, "body"}} = Store.add_note(store, item["id"], %{"body" => ""})

    assert {:error, {:invalid_work_item_id, "../escape"}} =
             Store.get_work_item(store, "../escape")

    assert {:error, {:work_item_not_found, "missing"}} = Store.get_work_item(store, "missing")
  end

  test "writes audit events for state-changing calls", %{store: store} do
    {:ok, org} = Store.create_org(store, %{"slug" => "false-systems", "name" => "False Systems"})

    {:ok, team} =
      Store.create_team(store, %{
        "org_id" => org["id"],
        "slug" => "platform",
        "name" => "Platform"
      })

    {:ok, item} =
      Store.create_work_item(store, %{
        "org_id" => org["id"],
        "team_id" => team["id"],
        "title" => "Task"
      })

    assert {:ok, events} = Store.audit_log(store)
    assert Enum.map(events, & &1["action"]) == ["org.created", "team.created", "work.created"]
    assert Enum.all?(events, &(&1["actor_type"] == "system"))
    assert List.last(events)["subject_id"] == item["id"]
  end

  defp id_sequence(ids) do
    {:ok, agent} = Agent.start_link(fn -> ids end)

    fn ->
      Agent.get_and_update(agent, fn
        [id | rest] -> {id, rest}
        [] -> flunk("id sequence exhausted")
      end)
    end
  end
end
