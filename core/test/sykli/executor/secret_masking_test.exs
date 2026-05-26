defmodule Sykli.Executor.SecretMaskingTest do
  use ExUnit.Case, async: false

  alias Sykli.Executor
  alias Sykli.Graph.Task
  alias Sykli.Graph.Task.CredentialBinding
  alias Sykli.Daemon.SessionStore

  setup do
    Application.delete_env(:sykli, :notification_service)
    Application.delete_env(:sykli, :notification_test_pid)
    Application.delete_env(:sykli, :secret_masking_test_pid)

    old_team_token = System.get_env("SYKLI_TEAM_TOKEN")

    on_exit(fn ->
      Application.delete_env(:sykli, :notification_service)
      Application.delete_env(:sykli, :notification_test_pid)
      Application.delete_env(:sykli, :secret_masking_test_pid)

      case old_team_token do
        nil -> System.delete_env("SYKLI_TEAM_TOKEN")
        value -> System.put_env("SYKLI_TEAM_TOKEN", value)
      end
    end)

    :ok
  end

  defmodule EchoEnvTarget do
    @behaviour Sykli.Target.Behaviour

    @impl true
    def name, do: "echo-env"

    @impl true
    def available?, do: {:ok, %{mode: :test}}

    @impl true
    def setup(opts), do: {:ok, %{workdir: Keyword.get(opts, :workdir, ".")}}

    @impl true
    def teardown(_state), do: :ok

    @impl true
    def resolve_secret(_name, _state), do: {:error, :not_found}

    @impl true
    def create_volume(_name, _opts, _state),
      do: {:ok, %{id: "mock", host_path: nil, reference: "mock"}}

    @impl true
    def artifact_path(_task, _artifact, _workdir, _state), do: "/mock/path"

    @impl true
    def copy_artifact(_src, _dest, _workdir, _state), do: :ok

    @impl true
    def start_services(_name, _services, _state), do: {:ok, nil}

    @impl true
    def stop_services(_info, _state), do: :ok

    @impl true
    def run_task(task, _state, _opts) do
      {:ok,
       "file=#{task.env["FILE_SECRET"]} literal=#{task.env["API_TOKEN"]} oidc=#{task.env["AWS_SESSION_TOKEN"]}"}
    end
  end

  defmodule EgressTarget do
    @behaviour Sykli.Target.Behaviour

    @impl true
    def name, do: "egress"

    @impl true
    def available?, do: {:ok, %{mode: :test}}

    @impl true
    def setup(opts), do: {:ok, %{workdir: Keyword.get(opts, :workdir, ".")}}

    @impl true
    def teardown(_state), do: :ok

    @impl true
    def resolve_secret(_name, _state), do: {:error, :not_found}

    @impl true
    def create_volume(_name, _opts, _state),
      do: {:ok, %{id: "mock", host_path: nil, reference: "mock"}}

    @impl true
    def artifact_path(_task, _artifact, _workdir, _state), do: "/mock/path"

    @impl true
    def copy_artifact(_src, _dest, _workdir, _state), do: :ok

    @impl true
    def start_services(_name, _services, _state), do: {:ok, nil}

    @impl true
    def stop_services(_info, _state), do: :ok

    @impl true
    def run_task(task, _state, _opts) do
      {:ok,
       "file=#{task.env["FILE_SECRET"]} literal=#{task.env["API_TOKEN"]} oidc=#{task.env["AWS_SESSION_TOKEN"]}"}
    end

    @impl true
    def evaluate_success_criteria(task, criteria, _state, _opts) do
      message =
        "criterion saw file=#{task.env["FILE_SECRET"]} literal=#{task.env["API_TOKEN"]} oidc=#{task.env["AWS_SESSION_TOKEN"]}"

      results =
        Enum.with_index(criteria || [], fn criterion, index ->
          %Sykli.SuccessCriteria.Result{
            index: index,
            type: criterion["type"],
            status: :failed,
            message: message,
            target: "egress"
          }
        end)

      error = %Sykli.Error{
        code: "success_criteria_failed",
        type: :execution,
        message: message,
        output: message
      }

      {:error, error, results}
    end
  end

  defmodule FakeOIDCService do
    def exchange(%{oidc: nil}, _state), do: {:ok, %{}}

    def exchange(%{oidc: %Sykli.Graph.Task.CredentialBinding{}}, _state) do
      {:ok, %{"AWS_SESSION_TOKEN" => "oidc-session-token-789"}}
    end

    def cleanup_temp_files, do: :ok
  end

  defmodule NotificationProbe do
    def notify(payload) do
      send(Application.fetch_env!(:sykli, :notification_test_pid), {:notification, payload})
      :ok
    end
  end

  defmodule CoordinatorCapture do
    import Plug.Conn

    def init(opts), do: opts

    def call(conn, _opts) do
      {:ok, body, conn} = read_body(conn)
      decoded = Jason.decode!(body)
      send(Application.fetch_env!(:sykli, :secret_masking_test_pid), {:run_summary_wire, body})

      run_id = get_in(decoded, ["run", "id"]) || "run_001"

      conn
      |> put_resp_content_type("application/json")
      |> send_resp(201, Jason.encode!(%{"ok" => true, "data" => %{"run" => %{"id" => run_id}}}))
    end
  end

  test "task results carry file-sourced, literal, and OIDC secret values for later masking" do
    file_secret = "file-secret-value-123"
    literal_secret = "literal-token-value-456"
    oidc_secret = "oidc-session-token-789"
    secret_path = Path.join(File.cwd!(), "tmp_secret_#{System.unique_integer([:positive])}")

    File.write!(secret_path, file_secret)
    on_exit(fn -> File.rm(secret_path) end)

    task = %Task{
      name: "build",
      command: "echo secrets",
      depends_on: [],
      env: %{"API_TOKEN" => literal_secret},
      secret_refs: [%{name: "FILE_SECRET", source: "file", key: secret_path}],
      oidc: %CredentialBinding{provider: :aws, role_arn: "arn:aws:iam::123456789012:role/test"}
    }

    assert {:ok, [result]} =
             Executor.run([task], %{"build" => task},
               target: EchoEnvTarget,
               workdir: File.cwd!(),
               oidc_service: FakeOIDCService
             )

    assert result.output == "file=#{file_secret} literal=#{literal_secret} oidc=#{oidc_secret}"

    assert Enum.sort(result.secret_values) ==
             Enum.sort([file_secret, literal_secret, oidc_secret])
  end

  test "run-scoped secrets are masked in occurrence, webhook, run-summary wire, and attestation" do
    workdir =
      Path.join(System.tmp_dir!(), "sykli-secret-egress-#{System.unique_integer([:positive])}")

    File.mkdir_p!(workdir)
    on_exit(fn -> File.rm_rf(workdir) end)

    file_secret = "file-secret-value-123"
    literal_secret = "literal-token-value-456"
    oidc_secret = "oidc-session-token-789"
    secrets = [file_secret, literal_secret, oidc_secret]

    port = free_port()
    {:ok, server} = Bandit.start_link(plug: CoordinatorCapture, port: port, startup_log: false)

    on_exit(fn -> Process.exit(server, :normal) end)

    Application.put_env(:sykli, :notification_service, NotificationProbe)
    Application.put_env(:sykli, :notification_test_pid, self())
    Application.put_env(:sykli, :secret_masking_test_pid, self())
    System.put_env("SYKLI_TEAM_TOKEN", "team-token")

    File.write!(Path.join(workdir, "secret.txt"), file_secret)

    {:ok, _session} =
      SessionStore.write(
        %{
          "coordinator" => "http://127.0.0.1:#{port}",
          "session_id" => "sess_001",
          "org_slug" => "false-systems",
          "team_slug" => "platform",
          "team_id" => "team_001"
        },
        path: Path.join(workdir, ".sykli")
      )

    json =
      Jason.encode!(%{
        "version" => "3",
        "tasks" => [
          %{
            "name" => "deploy",
            "command" =>
              "deploy --file #{file_secret} --literal #{literal_secret} --oidc #{oidc_secret}",
            "env" => %{"API_TOKEN" => literal_secret},
            "secret_refs" => [
              %{"name" => "FILE_SECRET", "source" => "file", "key" => "secret.txt"}
            ],
            "oidc" => %{
              "provider" => "aws",
              "role_arn" => "arn:aws:iam::123456789012:role/test"
            },
            "success_criteria" => [%{"type" => "exit_code", "equals" => 0}]
          }
        ]
      })

    File.write!(Path.join(workdir, "sykli.exs"), "IO.puts(#{inspect(json)})")

    result =
      File.cd!(workdir, fn ->
        Sykli.run(workdir, target: EgressTarget, oidc_service: FakeOIDCService)
      end)

    assert {:error, [task_result]} = result
    assert Enum.sort(task_result.secret_values) == Enum.sort(secrets)

    occurrence_json = File.read!(Path.join([workdir, ".sykli", "occurrence.json"]))
    assert_masked(occurrence_json, secrets)

    assert_received {:notification, notification_payload}
    assert_masked(Jason.encode!(notification_payload), secrets)

    assert_receive {:run_summary_wire, wire_body}, 2_000
    assert_masked(wire_body, secrets)

    envelope =
      Path.join([workdir, ".sykli", "attestation.json"])
      |> File.read!()
      |> Jason.decode!()

    assert {:ok, attestation} = Sykli.Attestation.Envelope.decode_payload(envelope)
    assert_masked(Jason.encode!(attestation), secrets)
  end

  defp assert_masked(serialized, secrets) do
    Enum.each(secrets, fn secret -> refute serialized =~ secret end)
    assert serialized =~ "***MASKED***"
  end

  defp free_port do
    {:ok, socket} = :gen_tcp.listen(0, [:binary, active: false])
    {:ok, port} = :inet.port(socket)
    :gen_tcp.close(socket)
    port
  end
end
