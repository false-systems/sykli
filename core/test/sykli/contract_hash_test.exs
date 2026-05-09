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
end
