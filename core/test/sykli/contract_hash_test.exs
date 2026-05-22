defmodule Sykli.ContractHashTest do
  use ExUnit.Case, async: true

  @moduletag :tmp_dir

  test "same bytes produce the same sha256 contract hash" do
    hash = Sykli.ContractHash.from_bytes(~s({"version":"1","tasks":[]}))

    assert hash == Sykli.ContractHash.from_bytes(~s({"version":"1","tasks":[]}))
    assert String.starts_with?(hash, "sha256:")
    assert String.length(hash) == 71
  end

  test "changed bytes produce a different contract hash" do
    hash_a = Sykli.ContractHash.from_bytes(~s({"version":"1","tasks":[]}))
    hash_b = Sykli.ContractHash.from_bytes(~s({"version":"1","tasks":[{"name":"test"}]}))

    refute hash_a == hash_b
  end

  test "canonicalizes emitted JSON before hashing" do
    json_a = ~s({"version":"1","tasks":[{"name":"test","command":"echo ok"}]})

    json_b = """
    {
      "version": "1",
      "tasks": [
        {
          "name": "test",
          "command": "echo ok"
        }
      ]
    }
    """

    assert {:ok, hash_a} = Sykli.ContractHash.from_json(json_a)
    assert {:ok, hash_b} = Sykli.ContractHash.from_json(json_b)
    assert hash_a == hash_b
  end

  test "hash is independent of object key order" do
    forward = ~s({"version":"1","tasks":[{"name":"t","command":"echo ok"}]})
    shuffled = ~s({"tasks":[{"command":"echo ok","name":"t"}],"version":"1"})

    assert {:ok, hash_a} = Sykli.ContractHash.from_json(forward)
    assert {:ok, hash_b} = Sykli.ContractHash.from_json(shuffled)
    assert hash_a == hash_b
  end

  test "hash is stable for large objects regardless of key order (>32 keys)" do
    keys = for i <- 1..40, do: "k#{i}"

    forward = keys |> Enum.map(&{&1, &1}) |> Jason.OrderedObject.new() |> Jason.encode!()

    reverse =
      keys
      |> Enum.reverse()
      |> Enum.map(&{&1, &1})
      |> Jason.OrderedObject.new()
      |> Jason.encode!()

    # The two inputs genuinely differ in key order on the wire...
    refute forward == reverse
    # ...but canonicalization sorts keys, so the hashes must match. Maps with
    # >32 keys use a hashed representation whose iteration order is not stable
    # across OTP versions — this is the case the canonicalize/1 sort guards.
    assert {:ok, hash_a} = Sykli.ContractHash.from_json(forward)
    assert {:ok, hash_b} = Sykli.ContractHash.from_json(reverse)
    assert hash_a == hash_b
  end
end
