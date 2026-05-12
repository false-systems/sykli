defmodule Sykli.OutboxTest do
  use ExUnit.Case, async: true
  @moduletag :tmp_dir

  @payload %{"version" => "1", "run" => %{"id" => "run_001"}}

  test "enqueue and drain deletes successful entries", %{tmp_dir: tmp_dir} do
    assert :ok = Sykli.Outbox.enqueue("runs", @payload, path: tmp_dir)
    assert {:ok, 1} = Sykli.Outbox.pending_count("runs", path: tmp_dir)

    assert {:ok, 1} =
             Sykli.Outbox.drain(
               "runs",
               fn payload ->
                 send(self(), {:sent, payload})
                 :ok
               end, path: tmp_dir)

    assert_received {:sent, @payload}
    assert {:ok, 0} = Sykli.Outbox.pending_count("runs", path: tmp_dir)
  end

  test "drain stops on first error and leaves failed file", %{tmp_dir: tmp_dir} do
    :ok = Sykli.Outbox.enqueue("runs", @payload, path: tmp_dir)

    assert {:error, 0, :nope} =
             Sykli.Outbox.drain("runs", fn _payload -> {:error, :nope} end, path: tmp_dir)

    assert {:ok, 1} = Sykli.Outbox.pending_count("runs", path: tmp_dir)
  end

  test "rejects path escaping kind", %{tmp_dir: tmp_dir} do
    assert {:error, :team_outbox_invalid_kind} =
             Sykli.Outbox.enqueue("../runs", @payload, path: tmp_dir)
  end
end
