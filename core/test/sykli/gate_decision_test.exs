defmodule Sykli.GateDecisionTest do
  use ExUnit.Case, async: true

  alias Sykli.GateDecision

  @now "2026-05-09T10:00:00Z"
  @later "2026-05-09T10:05:00Z"

  describe "new/1" do
    test "creates a waiting gate with versioned persisted shape" do
      assert {:ok, gate} =
               GateDecision.new(
                 id: "gate_001",
                 work_item_id: "work_001",
                 run_id: "run-001",
                 node_id: "approve-deploy",
                 requested_by_type: "system",
                 requested_by_id: "executor",
                 evidence_refs: [%{"type" => "occurrence", "uri" => "occ://1"}],
                 now: @now
               )

      assert gate.id == "gate_001"
      assert gate.version == "1"
      assert gate.status == "waiting"
      assert gate.created_at == @now
      assert gate.updated_at == @now

      assert GateDecision.to_map(gate) == %{
               "id" => "gate_001",
               "version" => "1",
               "work_item_id" => "work_001",
               "run_id" => "run-001",
               "node_id" => "approve-deploy",
               "status" => "waiting",
               "reason" => nil,
               "requested_by_type" => "system",
               "requested_by_id" => "executor",
               "decided_by" => nil,
               "decided_at" => nil,
               "created_at" => @now,
               "updated_at" => @now,
               "evidence_refs" => [%{"type" => "occurrence", "uri" => "occ://1"}]
             }
    end
  end

  describe "decision transitions" do
    test "approves a waiting gate" do
      assert {:ok, gate} = GateDecision.new(id: "gate_001", now: @now)

      assert {:ok, approved} =
               GateDecision.approve(gate, "Evidence reviewed",
                 decided_by: "member:yair",
                 now: @later
               )

      assert approved.status == "approved"
      assert approved.reason == "Evidence reviewed"
      assert approved.decided_by == "member:yair"
      assert approved.decided_at == @later
      assert approved.updated_at == @later
    end

    test "rejects a waiting gate" do
      assert {:ok, gate} = GateDecision.new(id: "gate_001", now: @now)
      assert {:ok, rejected} = GateDecision.reject(gate, "Not safe", now: @later)
      assert rejected.status == "rejected"
      assert rejected.reason == "Not safe"
    end

    test "allows blocked gates to be resolved explicitly" do
      assert {:ok, gate} = GateDecision.new(id: "gate_001", status: "blocked", now: @now)
      assert {:ok, approved} = GateDecision.approve(gate, "Unblocked", now: @later)
      assert approved.status == "approved"
    end

    test "supports block and expire transitions for future runtime hooks" do
      assert {:ok, gate} = GateDecision.new(id: "gate_001", now: @now)
      assert {:ok, blocked} = GateDecision.block(gate, now: @later)
      assert blocked.status == "blocked"
      assert blocked.updated_at == @later

      assert {:ok, expired} = GateDecision.expire(blocked, now: "2026-05-09T10:10:00Z")
      assert expired.status == "expired"

      assert {:error, {:invalid_gate_transition, "expired", "approved"}} =
               GateDecision.approve(expired, "Too late")
    end

    test "rejects terminal transitions" do
      assert {:ok, gate} = GateDecision.new(id: "gate_001", now: @now)
      assert {:ok, approved} = GateDecision.approve(gate, "Reviewed", now: @later)

      assert {:error, {:invalid_gate_transition, "approved", "rejected"}} =
               GateDecision.reject(approved, "Changed mind")

      assert {:ok, expired} = GateDecision.new(id: "gate_002", status: "expired", now: @now)

      assert {:error, {:invalid_gate_transition, "expired", "approved"}} =
               GateDecision.approve(expired, "Too late")
    end

    test "requires a non-empty reason" do
      assert {:ok, gate} = GateDecision.new(id: "gate_001", now: @now)
      assert {:error, :gate_decision_missing_reason} = GateDecision.approve(gate, "")
      assert {:error, :gate_decision_missing_reason} = GateDecision.reject(gate, nil)
    end
  end

  describe "from_map/1" do
    test "loads persisted JSON-compatible maps" do
      assert {:ok, gate} =
               GateDecision.from_map(%{
                 "id" => "gate_001",
                 "version" => "1",
                 "status" => "approved",
                 "work_item_id" => "work_001",
                 "run_id" => "run-001",
                 "node_id" => "approve",
                 "reason" => "Looks safe",
                 "requested_by_type" => "daemon",
                 "requested_by_id" => "worker-1",
                 "decided_by" => "member:yair",
                 "decided_at" => @later,
                 "created_at" => @now,
                 "updated_at" => @later,
                 "evidence_refs" => [%{"type" => "attestation", "uri" => "local://att"}]
               })

      assert gate.id == "gate_001"
      assert gate.work_item_id == "work_001"
      assert gate.status == "approved"
      assert gate.evidence_refs == [%{"type" => "attestation", "uri" => "local://att"}]
    end

    test "rejects invalid ids, versions, statuses, requesters, and evidence refs" do
      assert {:error, {:invalid_gate_id, "../escape"}} =
               GateDecision.from_map(%{"id" => "../escape", "version" => "1"})

      assert {:error, {:missing_gate_version, nil}} =
               GateDecision.from_map(%{"id" => "gate_001"})

      assert {:error, {:unsupported_gate_version, "2"}} =
               GateDecision.from_map(%{"id" => "gate_001", "version" => "2"})

      assert {:error, {:invalid_gate_status, "done"}} =
               GateDecision.from_map(%{"id" => "gate_001", "version" => "1", "status" => "done"})

      assert {:error, {:invalid_gate_requester, :missing_type}} =
               GateDecision.from_map(%{
                 "id" => "gate_001",
                 "version" => "1",
                 "requested_by_id" => "executor"
               })

      assert {:error, {:invalid_gate_evidence_ref, "raw log"}} =
               GateDecision.from_map(%{
                 "id" => "gate_001",
                 "version" => "1",
                 "evidence_refs" => ["raw log"]
               })
    end
  end
end
