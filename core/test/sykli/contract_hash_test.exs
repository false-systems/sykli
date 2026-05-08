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

  test "hashes SDK file bytes", %{tmp_dir: tmp_dir} do
    path = Path.join(tmp_dir, "sykli.exs")
    json = Jason.encode!(%{"version" => "1", "tasks" => []})
    File.write!(path, "IO.puts(#{inspect(json)})")

    assert {:ok, hash} = Sykli.ContractHash.from_sdk_file(path)
    assert hash == Sykli.ContractHash.from_bytes(File.read!(path))
  end
end
