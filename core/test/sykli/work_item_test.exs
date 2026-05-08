defmodule Sykli.WorkItemTest do
  use ExUnit.Case, async: true

  alias Sykli.WorkItem

  @now "2026-05-08T10:00:00Z"

  describe "new/2" do
    test "creates an open work item with versioned persisted shape" do
      assert {:ok, item} =
               WorkItem.new("Investigate failing checkout deploy",
                 id: "work_001",
                 intent: "Find the failing deploy step",
                 created_by_type: "member",
                 created_by_id: "yair",
                 now: @now
               )

      assert item.id == "work_001"
      assert item.version == "1"
      assert item.status == "open"
      assert item.created_at == @now
      assert item.updated_at == @now

      assert WorkItem.to_map(item) == %{
               "id" => "work_001",
               "version" => "1",
               "title" => "Investigate failing checkout deploy",
               "intent" => "Find the failing deploy step",
               "status" => "open",
               "created_by_type" => "member",
               "created_by_id" => "yair",
               "assigned_to_type" => nil,
               "assigned_to_id" => nil,
               "created_at" => @now,
               "updated_at" => @now,
               "notes" => []
             }
    end

    test "rejects empty titles" do
      assert {:error, {:invalid_title, :empty}} = WorkItem.new("   ", id: "work_001")
    end
  end

  describe "status validation" do
    test "accepts only v0 work item statuses" do
      assert WorkItem.statuses() == ~w(open claimed running blocked done failed cancelled)

      assert {:ok, item} = WorkItem.new("Review PR", id: "work_001", now: @now)
      assert {:ok, updated} = WorkItem.update_status(item, "blocked", now: @now)
      assert updated.status == "blocked"

      assert {:error, {:invalid_work_item_status, "waiting"}} =
               WorkItem.update_status(item, "waiting")
    end
  end

  describe "claim/4" do
    test "claims a work item for a member, agent, or daemon" do
      assert {:ok, item} = WorkItem.new("Review PR", id: "work_001", now: @now)

      assert {:ok, claimed} = WorkItem.claim(item, "agent", "claude", now: @now)
      assert claimed.status == "claimed"
      assert claimed.assigned_to_type == "agent"
      assert claimed.assigned_to_id == "claude"
    end

    test "rejects invalid assignment types" do
      assert {:ok, item} = WorkItem.new("Review PR", id: "work_001", now: @now)

      assert {:error, {:invalid_assignment_type, "robot"}} =
               WorkItem.claim(item, "robot", "claude")
    end

    test "does not silently overwrite an existing claim" do
      assert {:ok, item} = WorkItem.new("Review PR", id: "work_001", now: @now)
      assert {:ok, claimed} = WorkItem.claim(item, "agent", "claude", now: @now)

      assert {:error,
              {:work_item_already_claimed, "work_001",
               %{
                 "status" => "claimed",
                 "assigned_to_type" => "agent",
                 "assigned_to_id" => "claude"
               }}} = WorkItem.claim(claimed, "daemon", "yair-mbp", now: @now)
    end
  end

  describe "append_note/3" do
    test "appends a versioned local note" do
      assert {:ok, item} = WorkItem.new("Review PR", id: "work_001", now: @now)

      assert {:ok, noted} =
               WorkItem.append_note(item, "Found likely API breakage",
                 note_id: "note_001",
                 author_type: "member",
                 author_id: "yair",
                 now: @now
               )

      assert noted.notes == [
               %{
                 "id" => "note_001",
                 "author_type" => "member",
                 "author_id" => "yair",
                 "body" => "Found likely API breakage",
                 "created_at" => @now
               }
             ]
    end

    test "rejects empty notes" do
      assert {:ok, item} = WorkItem.new("Review PR", id: "work_001")
      assert {:error, {:invalid_note, :empty_body}} = WorkItem.append_note(item, " ")
    end
  end

  describe "from_map/1" do
    test "loads persisted JSON-compatible maps" do
      assert {:ok, item} =
               WorkItem.from_map(%{
                 "id" => "work_001",
                 "version" => "1",
                 "title" => "Review PR",
                 "status" => "open",
                 "created_by_type" => "member",
                 "created_by_id" => "yair",
                 "created_at" => @now,
                 "updated_at" => @now,
                 "notes" => []
               })

      assert item.id == "work_001"
      assert item.created_by_type == "member"
      assert item.created_by_id == "yair"
    end

    test "rejects invalid IDs and missing or unsupported versions" do
      assert {:error, {:invalid_work_item_id, "../escape"}} =
               WorkItem.from_map(%{"id" => "../escape", "title" => "Bad"})

      assert {:error, {:missing_work_item_version, nil}} =
               WorkItem.from_map(%{"id" => "work_001", "title" => "Bad"})

      assert {:error, {:unsupported_work_item_version, "2"}} =
               WorkItem.from_map(%{"id" => "work_001", "title" => "Bad", "version" => "2"})
    end

    test "rejects malformed created_by actor refs" do
      assert {:error, {:invalid_created_by, :missing_type}} =
               WorkItem.from_map(%{
                 "id" => "work_001",
                 "version" => "1",
                 "title" => "Bad",
                 "created_by_id" => "yair"
               })

      assert {:error, {:invalid_actor_type, "robot"}} =
               WorkItem.from_map(%{
                 "id" => "work_001",
                 "version" => "1",
                 "title" => "Bad",
                 "created_by_type" => "robot",
                 "created_by_id" => "yair"
               })
    end

    test "rejects malformed persisted notes" do
      base = %{
        "id" => "work_001",
        "version" => "1",
        "title" => "Review PR",
        "status" => "open"
      }

      assert {:error, {:invalid_note, 1}} =
               WorkItem.from_map(Map.put(base, "notes", [1]))

      assert {:error, {:invalid_note, {:id, nil}}} =
               WorkItem.from_map(Map.put(base, "notes", [%{"body" => "Missing id"}]))

      assert {:error, {:invalid_note_author, :missing_type}} =
               WorkItem.from_map(
                 Map.put(base, "notes", [
                   %{
                     "id" => "note_001",
                     "body" => "Bad author",
                     "author_id" => "yair",
                     "created_at" => @now
                   }
                 ])
               )
    end
  end
end
