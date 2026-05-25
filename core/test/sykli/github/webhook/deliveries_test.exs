defmodule Sykli.GitHub.Webhook.DeliveriesTest do
  use ExUnit.Case, async: false

  alias Sykli.GitHub.Webhook.Deliveries

  setup do
    Deliveries.clear()
    :ok
  end

  test "rejects duplicate delivery IDs" do
    assert :ok = Deliveries.accept("delivery-1", 1_000)
    assert {:error, :duplicate_delivery} = Deliveries.accept("delivery-1", 1_100)
  end

  test "expires old delivery IDs" do
    assert :ok = Deliveries.accept("delivery-1", 1_000, ttl_ms: 100)
    assert :ok = Deliveries.accept("delivery-1", 1_200, ttl_ms: 100)
  end

  test "trims oldest entries over the configured limit" do
    assert :ok = Deliveries.accept("a", 1_000, limit: 2)
    assert :ok = Deliveries.accept("b", 1_001, limit: 2)
    assert :ok = Deliveries.accept("c", 1_002, limit: 2)
    assert :ok = Deliveries.accept("a", 1_003, limit: 2)
  end

  test "evict removes a delivery_id so the next accept with the same id succeeds" do
    assert :ok = Deliveries.accept("delivery-evict-1", 1_000)
    assert {:error, :duplicate_delivery} = Deliveries.accept("delivery-evict-1", 1_100)

    assert :ok = Deliveries.evict("delivery-evict-1")
    assert :ok = Deliveries.accept("delivery-evict-1", 1_200)
  end

  test "evict is idempotent on missing keys and tolerant of non-binary input" do
    assert :ok = Deliveries.evict("never-inserted")
    assert :ok = Deliveries.evict("never-inserted")
    assert :ok = Deliveries.evict(nil)
    assert :ok = Deliveries.evict(:atom)
  end

  test "replay table is created at startup and owned by a long-lived process" do
    assert :ets.whereis(:sykli_github_webhook_deliveries) != :undefined

    owner = :ets.info(:sykli_github_webhook_deliveries, :owner)
    assert is_pid(owner) and Process.alive?(owner)
    # Owned by the application master (created in Sykli.Application.start/2), not
    # by this transient test process — so it outlives request/test processes and
    # replay protection is not silently reset when a caller exits.
    refute owner == self()
  end

  test "setup/0 is idempotent and does not reset existing entries" do
    assert :ok = Deliveries.setup()
    assert :ok = Deliveries.accept("setup-idempotent", 1_000)
    assert :ok = Deliveries.setup()
    assert {:error, :duplicate_delivery} = Deliveries.accept("setup-idempotent", 1_100)
  end
end
