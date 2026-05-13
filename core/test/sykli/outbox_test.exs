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
               end,
               path: tmp_dir
             )

    assert_received {:sent, @payload}
    assert {:ok, 0} = Sykli.Outbox.pending_count("runs", path: tmp_dir)
  end

  test "delete removes an enqueued payload idempotently", %{tmp_dir: tmp_dir} do
    assert :ok = Sykli.Outbox.enqueue("runs", @payload, path: tmp_dir)
    assert {:ok, 1} = Sykli.Outbox.pending_count("runs", path: tmp_dir)

    assert :ok = Sykli.Outbox.delete("runs", @payload, path: tmp_dir)
    assert :ok = Sykli.Outbox.delete("runs", @payload, path: tmp_dir)
    assert {:ok, 0} = Sykli.Outbox.pending_count("runs", path: tmp_dir)
  end

  test "drain stops on first error and leaves failed file", %{tmp_dir: tmp_dir} do
    :ok = Sykli.Outbox.enqueue("runs", @payload, path: tmp_dir)

    assert {:error, 0, :nope} =
             Sykli.Outbox.drain("runs", fn _payload -> {:error, :nope} end, path: tmp_dir)

    assert {:ok, 1} = Sykli.Outbox.pending_count("runs", path: tmp_dir)
  end

  test "drain skips permanent invalid payload responses and continues", %{tmp_dir: tmp_dir} do
    first = put_in(@payload, ["run", "id"], "run_001")
    second = put_in(@payload, ["run", "id"], "run_002")

    :ok = Sykli.Outbox.enqueue("runs", first, path: tmp_dir)
    :ok = Sykli.Outbox.enqueue("runs", second, path: tmp_dir)

    assert {:ok, 1} =
             Sykli.Outbox.drain(
               "runs",
               fn
                 %{"run" => %{"id" => "run_001"}} -> {:error, :team_run_invalid_payload}
                 %{"run" => %{"id" => "run_002"}} -> :ok
               end,
               path: tmp_dir
             )

    assert {:ok, 0} = Sykli.Outbox.pending_count("runs", path: tmp_dir)
  end

  test "drain skips malformed json and continues to newer files", %{tmp_dir: tmp_dir} do
    dir = Path.join([tmp_dir, ".sykli", "outbox", "runs"])
    File.mkdir_p!(dir)
    File.write!(Path.join(dir, "run_000.json"), "{not json")
    :ok = Sykli.Outbox.enqueue("runs", @payload, path: tmp_dir)

    assert {:ok, 1} = Sykli.Outbox.drain("runs", fn _payload -> :ok end, path: tmp_dir)
    assert {:ok, 0} = Sykli.Outbox.pending_count("runs", path: tmp_dir)
  end

  test "rejects path escaping kind", %{tmp_dir: tmp_dir} do
    assert {:error, :team_outbox_invalid_kind} =
             Sykli.Outbox.enqueue("../runs", @payload, path: tmp_dir)
  end
end
