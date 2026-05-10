defmodule Sykli.Daemon.SessionStoreTest do
  use ExUnit.Case, async: true

  import Bitwise

  alias Sykli.Daemon.SessionStore

  setup do
    tmp =
      Path.join(System.tmp_dir!(), "sykli-session-store-#{System.unique_integer([:positive])}")

    on_exit(fn -> File.rm_rf(tmp) end)
    {:ok, tmp: tmp}
  end

  test "writes and reloads session without token", %{tmp: tmp} do
    session = %{
      "coordinator" => "https://sykli.internal",
      "session_id" => "sess_001",
      "team_id" => "team_001",
      "token" => "secret"
    }

    assert {:ok, written} = SessionStore.write(Map.delete(session, "token"), path: tmp)
    assert written["version"] == 1

    assert {:ok, reloaded} = SessionStore.read(path: tmp)
    assert reloaded["session_id"] == "sess_001"
    refute Map.has_key?(reloaded, "token")

    {:ok, stat} = File.stat(SessionStore.path(path: tmp))
    assert (stat.mode &&& 0o077) == 0
  end

  test "malformed session returns clear error", %{tmp: tmp} do
    path = SessionStore.path(path: tmp)
    File.mkdir_p!(Path.dirname(path))
    File.write!(path, "{bad")

    assert {:error, :daemon_session_malformed} = SessionStore.read(path: tmp)
  end
end
