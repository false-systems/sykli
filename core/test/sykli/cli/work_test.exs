defmodule Sykli.CLI.WorkTest do
  use ExUnit.Case, async: true

  import ExUnit.CaptureIO

  alias Sykli.CLI.Work
  alias Sykli.Daemon.SessionStore
  alias Sykli.RunHistory

  @moduletag :tmp_dir

  @now "2026-05-08T10:00:00Z"

  describe "local work commands" do
    test "create prints human output and persists item", %{tmp_dir: tmp_dir} do
      output =
        capture_io(fn ->
          assert Work.run(["create", "Investigate deploy"],
                   path: tmp_dir,
                   id: "work_001",
                   now: @now
                 ) == 0
        end)

      assert output =~ "Created work item work_001"
      assert File.exists?(Path.join([tmp_dir, ".sykli", "work", "items", "work_001.json"]))
    end

    test "create --json returns shared envelope", %{tmp_dir: tmp_dir} do
      result =
        run_json(["create", "Investigate deploy", "--intent", "Find failure", "--json"],
          path: tmp_dir,
          id: "work_001",
          now: @now
        )

      assert result["ok"] == true
      assert result["data"]["source"] == "local"
      assert result["data"]["item"]["id"] == "work_001"
      assert result["data"]["item"]["title"] == "Investigate deploy"
      assert result["data"]["item"]["intent"] == "Find failure"
      assert result["data"]["item"]["created_by_type"] == "member"
      assert result["data"]["item"]["created_by_id"] == "test-user"
    end

    test "create supports equals-form flags and joins unquoted title words", %{tmp_dir: tmp_dir} do
      result =
        run_json(["create", "Investigate", "deploy", "--intent=Find failure", "--json"],
          path: tmp_dir,
          id: "work_001",
          now: @now
        )

      assert result["data"]["item"]["title"] == "Investigate deploy"
      assert result["data"]["item"]["intent"] == "Find failure"
    end

    test "list --json handles empty and deterministic lists", %{tmp_dir: tmp_dir} do
      assert run_json(["list", "--json"], path: tmp_dir)["data"]["items"] == []

      assert run_silent(["create", "Second"], path: tmp_dir, id: "work_b", now: @now) == 0
      assert run_silent(["create", "First"], path: tmp_dir, id: "work_a", now: @now) == 0

      result = run_json(["list", "--json"], path: tmp_dir)
      assert Enum.map(result["data"]["items"], & &1["id"]) == ["work_a", "work_b"]
    end

    test "show --json returns one work item", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0

      result = run_json(["show", "work_001", "--json"], path: tmp_dir)
      assert result["data"]["item"]["id"] == "work_001"
      assert result["data"]["item"]["status"] == "open"
    end

    test "claim --json updates assignment with default actor", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0

      result = run_json(["claim", "work_001", "--json"], path: tmp_dir, now: @now)
      item = result["data"]["item"]
      assert item["status"] == "claimed"
      assert item["assigned_to_type"] == "member"
      assert item["assigned_to_id"] == "test-user"
    end

    test "claim supports explicit actor with equals-form flag", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0

      result =
        run_json(["claim", "work_001", "--actor=agent:claude", "--json"],
          path: tmp_dir,
          now: @now
        )

      item = result["data"]["item"]
      assert item["assigned_to_type"] == "agent"
      assert item["assigned_to_id"] == "claude"
    end

    test "note --json appends note and returns note plus item", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0

      result =
        run_json(["note", "work_001", "Found likely API breakage", "--json"],
          path: tmp_dir,
          note_id: "note_001",
          now: @now
        )

      assert result["data"]["note"]["id"] == "note_001"
      assert result["data"]["note"]["body"] == "Found likely API breakage"
      assert [note] = result["data"]["item"]["notes"]
      assert note["id"] == "note_001"
    end

    test "note joins unquoted body words", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0

      result =
        run_json(["note", "work_001", "Found", "likely", "API", "breakage", "--json"],
          path: tmp_dir,
          note_id: "note_001",
          now: @now
        )

      assert result["data"]["note"]["body"] == "Found likely API breakage"
    end

    test "help exits successfully", %{tmp_dir: tmp_dir} do
      output =
        capture_io(fn ->
          assert Work.run(["--help"], path: tmp_dir) == 0
        end)

      assert output =~ "Usage: sykli work <command>"
      assert output =~ "Unknown flags are rejected"
    end

    test "runs --json lists associated runs", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0

      save_run(tmp_dir, %{
        id: "run-1",
        timestamp: ~U[2026-05-08 10:00:00Z],
        work_item_id: "work_001",
        contract_hash: "sha256:one"
      })

      save_run(tmp_dir, %{
        id: "run-2",
        timestamp: ~U[2026-05-08 11:00:00Z],
        work_item_id: "work_001",
        contract_hash: "sha256:two"
      })

      result = run_json(["runs", "work_001", "--json"], path: tmp_dir)

      assert result["data"]["source"] == "local"
      assert result["data"]["work_item_id"] == "work_001"
      assert Enum.map(result["data"]["runs"], & &1["id"]) == ["run-2", "run-1"]
      assert hd(result["data"]["runs"])["contract_hash"] == "sha256:two"
      assert hd(result["data"]["runs"])["timestamp"] == "2026-05-08T11:00:00Z"
    end
  end

  describe "team work commands" do
    setup %{tmp_dir: tmp_dir} do
      write_session(tmp_dir)
      :ok
    end

    test "create --team routes to coordinator and does not create local item", %{tmp_dir: tmp_dir} do
      result =
        run_json(["create", "Investigate deploy", "--team", "platform", "--json"],
          path: tmp_dir,
          work_client: __MODULE__.FakeTeamClient,
          team_token: "secret",
          now: @now
        )

      assert result["data"]["source"] == "team"
      assert result["data"]["team"] == "platform"
      assert result["data"]["item"]["id"] == "work_team_001"
      refute File.exists?(Path.join([tmp_dir, ".sykli", "work", "items", "work_team_001.json"]))

      assert_received {:create_team_work, "secret",
                       %{"title" => "Investigate deploy", "created_by" => "member:test-user"}}
    end

    test "list show claim and note --team return team JSON", %{tmp_dir: tmp_dir} do
      assert run_json(["list", "--team", "platform", "--json"],
               path: tmp_dir,
               work_client: __MODULE__.FakeTeamClient,
               team_token: "secret"
             )["data"]["items"] == [%{"id" => "work_team_001", "status" => "open"}]

      assert run_json(["show", "work_team_001", "--team", "platform", "--json"],
               path: tmp_dir,
               work_client: __MODULE__.FakeTeamClient,
               team_token: "secret"
             )["data"]["item"]["id"] == "work_team_001"

      claim =
        run_json(["claim", "work_team_001", "--team", "platform", "--json"],
          path: tmp_dir,
          work_client: __MODULE__.FakeTeamClient,
          team_token: "secret"
        )

      assert claim["data"]["item"]["status"] == "claimed"
      assert claim["data"]["item"]["assigned_to_id"] == "test-user"

      note =
        run_json(["note", "work_team_001", "Found issue", "--team", "platform", "--json"],
          path: tmp_dir,
          work_client: __MODULE__.FakeTeamClient,
          team_token: "secret"
        )

      assert note["data"]["note"]["body"] == "Found issue"
      assert note["data"]["source"] == "team"
    end

    test "team mode without joined session fails and does not fall back locally", %{
      tmp_dir: tmp_dir
    } do
      File.rm!(SessionStore.path(path: tmp_dir))

      result =
        run_json(["create", "Investigate deploy", "--team", "platform", "--json"],
          path: tmp_dir,
          work_client: __MODULE__.FakeTeamClient,
          team_token: "secret",
          expect: 1
        )

      assert result["error"]["code"] == "work.team_not_joined"
      assert {:ok, []} = Sykli.Work.Store.list(path: tmp_dir)
    end

    test "team mismatch and missing token fail clearly", %{tmp_dir: tmp_dir} do
      mismatch =
        run_json(["list", "--team", "other", "--json"],
          path: tmp_dir,
          work_client: __MODULE__.FakeTeamClient,
          team_token: "secret",
          expect: 1
        )

      assert mismatch["error"]["code"] == "work.team_mismatch"

      missing_token =
        run_json(["list", "--team", "platform", "--json"],
          path: tmp_dir,
          work_client: __MODULE__.FakeTeamClient,
          expect: 1
        )

      assert missing_token["error"]["code"] == "work.team_missing_token"
    end

    test "coordinator unavailable and unauthorized are structured without token leakage", %{
      tmp_dir: tmp_dir
    } do
      unavailable =
        run_json(["list", "--team", "platform", "--json"],
          path: tmp_dir,
          work_client: __MODULE__.UnavailableTeamClient,
          team_token: "super-secret",
          expect: 1
        )

      assert unavailable["error"]["code"] == "work.team_coordinator_unavailable"
      refute Jason.encode!(unavailable) =~ "super-secret"

      unauthorized =
        run_json(["list", "--team", "platform", "--json"],
          path: tmp_dir,
          work_client: __MODULE__.UnauthorizedTeamClient,
          team_token: "super-secret",
          expect: 1
        )

      assert unauthorized["error"]["code"] == "work.team_unauthorized"
      refute Jason.encode!(unauthorized) =~ "super-secret"
    end

    test "team runs command is explicitly unsupported before run sync", %{tmp_dir: tmp_dir} do
      result =
        run_json(["runs", "work_team_001", "--team", "platform", "--json"],
          path: tmp_dir,
          work_client: __MODULE__.FakeTeamClient,
          team_token: "secret",
          expect: 1
        )

      assert result["error"]["code"] == "work.team_runs_not_supported"
    end
  end

  describe "errors" do
    test "create without title returns structured JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["create", "--json"], path: tmp_dir, expect: 1)
      assert result["ok"] == false
      assert result["error"]["code"] == "work_item_missing_title"
    end

    test "unknown command returns clear JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["wat", "--json"], path: tmp_dir, expect: 1)
      assert result["ok"] == false
      assert result["error"]["code"] == "invalid_work_item"
      assert result["error"]["message"] == "invalid local work item: invalid work command \"wat\""
    end

    test "unknown flag returns clear JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["list", "--bogus", "--json"], path: tmp_dir, expect: 1)
      assert result["ok"] == false
      assert result["error"]["code"] == "invalid_work_item"
      assert result["error"]["message"] == "invalid local work item: unknown flag --bogus"
    end

    test "invalid actor type returns clear JSON error without raw tuple text", %{tmp_dir: tmp_dir} do
      result =
        run_json(["claim", "work_001", "--actor", "robot:r2d2", "--json"],
          path: tmp_dir,
          expect: 1
        )

      assert result["ok"] == false
      assert result["error"]["code"] == "invalid_work_item"
      assert result["error"]["message"] == "invalid local work item: invalid actor type \"robot\""
      refute result["error"]["message"] =~ "{:"
    end

    test "invalid id returns structured JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["show", "../escape", "--json"], path: tmp_dir, expect: 1)
      assert result["ok"] == false
      assert result["error"]["code"] == "invalid_work_item_id"
    end

    test "not found returns structured JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["show", "missing", "--json"], path: tmp_dir, expect: 1)
      assert result["ok"] == false
      assert result["error"]["code"] == "work_item_not_found"
    end

    test "already claimed returns structured JSON error", %{tmp_dir: tmp_dir} do
      assert run_silent(["create", "Review PR"], path: tmp_dir, id: "work_001", now: @now) == 0
      assert run_silent(["claim", "work_001"], path: tmp_dir, now: @now) == 0

      result = run_json(["claim", "work_001", "--json"], path: tmp_dir, expect: 1)
      assert result["error"]["code"] == "work_item_already_claimed"
    end

    test "malformed persisted JSON returns structured JSON error", %{tmp_dir: tmp_dir} do
      dir = Path.join([tmp_dir, ".sykli", "work", "items"])
      File.mkdir_p!(dir)
      File.write!(Path.join(dir, "work_001.json"), "{bad json")

      result = run_json(["show", "work_001", "--json"], path: tmp_dir, expect: 1)
      assert result["error"]["code"] == "malformed_work_item_json"
    end

    test "runs for missing work item returns structured JSON error", %{tmp_dir: tmp_dir} do
      result = run_json(["runs", "missing", "--json"], path: tmp_dir, expect: 1)
      assert result["error"]["code"] == "work_item_not_found"
    end
  end

  defp run_json(args, opts) do
    opts = Keyword.put_new(opts, :default_actor_id, "test-user")
    expected_code = Keyword.get(opts, :expect, 0)

    output =
      capture_io(fn ->
        assert Work.run(args, opts) == expected_code
      end)

    Jason.decode!(output)
  end

  defp run_silent(args, opts) do
    opts = Keyword.put_new(opts, :default_actor_id, "test-user")

    capture_io(fn ->
      assert Work.run(args, opts) == 0
    end)

    0
  end

  defp save_run(tmp_dir, attrs) do
    run = %RunHistory.Run{
      id: attrs.id,
      timestamp: attrs.timestamp,
      git_ref: "abc123",
      git_branch: "main",
      work_item_id: attrs.work_item_id,
      contract_hash: attrs.contract_hash,
      tasks: [%RunHistory.TaskResult{name: "test", status: :passed, duration_ms: 10}],
      overall: :passed
    }

    :ok = RunHistory.save(run, path: tmp_dir)
  end

  defp write_session(tmp_dir) do
    {:ok, _session} =
      SessionStore.write(
        %{
          "coordinator" => "https://sykli.internal",
          "org" => "false-systems",
          "team" => "platform",
          "daemon_id" => "test-daemon",
          "session_id" => "sess_001",
          "team_id" => "team_001",
          "heartbeat_interval_seconds" => 15,
          "policy" => %{"upload_raw_logs_by_default" => false},
          "labels" => [],
          "capabilities" => ["local"],
          "accepts_remote_work" => false,
          "joined_at" => @now
        },
        path: tmp_dir
      )
  end

  defmodule FakeTeamClient do
    def create(session, token, attrs, _opts) do
      send(self(), {:create_team_work, token, attrs})

      {:ok,
       %{
         "id" => "work_team_001",
         "team_id" => session["team_id"],
         "title" => attrs["title"],
         "status" => "open"
       }}
    end

    def list(_session, _token, _opts) do
      {:ok, [%{"id" => "work_team_001", "status" => "open"}]}
    end

    def show(_session, _token, "work_team_001", _opts) do
      {:ok, %{"id" => "work_team_001", "status" => "open", "title" => "Investigate deploy"}}
    end

    def claim(_session, _token, "work_team_001", attrs, _opts) do
      {:ok,
       %{
         "id" => "work_team_001",
         "status" => "claimed",
         "assigned_to_type" => attrs["assigned_to_type"],
         "assigned_to_id" => attrs["assigned_to_id"]
       }}
    end

    def note(_session, _token, "work_team_001", attrs, _opts) do
      {:ok, %{"id" => "note_001", "work_item_id" => "work_team_001", "body" => attrs["body"]}}
    end
  end

  defmodule UnavailableTeamClient do
    def list(_session, _token, _opts),
      do: {:error, {:team_coordinator_unavailable, :econnrefused}}
  end

  defmodule UnauthorizedTeamClient do
    def list(_session, _token, _opts), do: {:error, :team_unauthorized}
  end
end
