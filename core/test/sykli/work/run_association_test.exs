defmodule Sykli.Work.RunAssociationTest do
  use ExUnit.Case, async: true

  alias Sykli.RunHistory
  alias Sykli.Work.Store

  @moduletag :tmp_dir

  test "Sykli.run persists work_item_id and contract_hash in run history", %{tmp_dir: tmp_dir} do
    write_pipeline(tmp_dir, "echo associated")
    {:ok, _item} = Store.create("Associated run", path: tmp_dir, id: "work_001")
    {:ok, sdk_file} = detected_sdk_file(tmp_dir)
    {:ok, contract_hash} = Sykli.ContractHash.from_sdk_file(sdk_file)

    assert {:ok, _results} =
             Sykli.run(tmp_dir, work_item_id: "work_001", contract_hash: contract_hash)

    assert {:ok, run} = RunHistory.load_latest(path: tmp_dir)
    assert run.work_item_id == "work_001"
    assert run.contract_hash == contract_hash
    assert run.overall == :passed
  end

  test "normal runs without work metadata preserve previous history shape", %{tmp_dir: tmp_dir} do
    write_pipeline(tmp_dir, "echo plain")

    assert {:ok, _results} = Sykli.run(tmp_dir)

    assert {:ok, run} = RunHistory.load_latest(path: tmp_dir)
    assert run.work_item_id == nil
    assert run.contract_hash == nil
  end

  test "same contract gets the same hash and changed contract gets a different hash", %{
    tmp_dir: tmp_dir
  } do
    write_pipeline(tmp_dir, "echo one")
    {:ok, sdk_file} = detected_sdk_file(tmp_dir)
    {:ok, hash_a} = Sykli.ContractHash.from_sdk_file(sdk_file)
    {:ok, hash_b} = Sykli.ContractHash.from_sdk_file(sdk_file)

    write_pipeline(tmp_dir, "echo two")
    {:ok, hash_c} = Sykli.ContractHash.from_sdk_file(sdk_file)

    assert hash_a == hash_b
    refute hash_a == hash_c
  end

  defp write_pipeline(tmp_dir, command) do
    json =
      Jason.encode!(%{"version" => "1", "tasks" => [%{"name" => "test", "command" => command}]})

    File.write!(
      Path.join(tmp_dir, "sykli.exs"),
      "IO.puts(#{inspect(json)})"
    )
  end

  defp detected_sdk_file(tmp_dir) do
    with {:ok, {sdk_file, _runner}} <- Sykli.Detector.find(tmp_dir) do
      {:ok, sdk_file}
    end
  end
end
