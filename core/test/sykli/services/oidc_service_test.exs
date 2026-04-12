defmodule Sykli.Services.OIDCServiceTest do
  use ExUnit.Case, async: false

  alias Sykli.Services.OIDCService

  describe "exchange/2" do
    test "returns {:ok, %{}} when task oidc is nil" do
      task = %Sykli.Graph.Task{oidc: nil}
      assert {:ok, %{}} = OIDCService.exchange(task, %{})
    end
  end

  describe "acquire_identity_token (via exchange path)" do
    setup do
      env_vars = [
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        "CI_JOB_JWT_V2"
      ]

      saved = Enum.map(env_vars, fn var -> {var, System.get_env(var)} end)

      on_exit(fn ->
        Enum.each(saved, fn
          {var, nil} -> System.delete_env(var)
          {var, val} -> System.put_env(var, val)
        end)
      end)

      Enum.each(env_vars, &System.delete_env/1)
      :ok
    end

    test "returns error when no CI environment is detected" do
      binding = %Sykli.Graph.Task.CredentialBinding{
        provider: :aws,
        audience: "sykli",
        role_arn: "arn:aws:iam::123456789:role/test"
      }

      task = %Sykli.Graph.Task{oidc: binding}

      assert {:error, msg} = OIDCService.exchange(task, %{})
      assert msg =~ "OIDC not available"
      assert msg =~ "GitHub Actions"
    end

    test "GitLab path is selected when CI_JOB_JWT_V2 is set" do
      # Set GitLab env var with a deliberately malformed token
      # to verify the GitLab acquisition path is chosen (not GitHub)
      # without making any network calls
      System.put_env("CI_JOB_JWT_V2", "not-a-real-jwt")

      binding = %Sykli.Graph.Task.CredentialBinding{
        provider: :aws,
        audience: "sykli",
        role_arn: "arn:aws:iam::123456789:role/test"
      }

      task = %Sykli.Graph.Task{oidc: binding}

      # Should fail at JWT verification (malformed), not at token acquisition
      result = OIDCService.exchange(task, %{})
      assert {:error, msg} = result
      # The error should NOT be "OIDC not available" — proving GitLab path was taken
      refute msg =~ "OIDC not available"
    end

    test "GitHub path requires ACTIONS_ID_TOKEN_REQUEST_TOKEN" do
      System.put_env("ACTIONS_ID_TOKEN_REQUEST_URL", "https://example.com/token")
      # Deliberately not setting ACTIONS_ID_TOKEN_REQUEST_TOKEN

      binding = %Sykli.Graph.Task.CredentialBinding{
        provider: :aws,
        audience: "sykli",
        role_arn: "arn:aws:iam::123456789:role/test"
      }

      task = %Sykli.Graph.Task{oidc: binding}

      assert {:error, msg} = OIDCService.exchange(task, %{})
      assert msg =~ "ACTIONS_ID_TOKEN_REQUEST_TOKEN"
    end
  end

  describe "cleanup_temp_files/0" do
    test "returns :ok when no temp files tracked" do
      Process.delete(:sykli_oidc_temp_files)
      assert :ok = OIDCService.cleanup_temp_files()
    end

    test "removes tracked temp files and clears process dict" do
      path = Path.join(System.tmp_dir!(), "sykli-oidc-test-#{:rand.uniform(100_000)}")
      File.write!(path, "test")

      Process.put(:sykli_oidc_temp_files, [path])

      assert :ok = OIDCService.cleanup_temp_files()
      refute File.exists?(path)
      assert Process.get(:sykli_oidc_temp_files) == nil
    end
  end
end
