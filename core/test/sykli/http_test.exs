defmodule Sykli.HTTPTest do
  use ExUnit.Case, async: false

  alias Sykli.HTTP

  describe "check_token_transport/1" do
    setup do
      System.delete_env("SYKLI_COORDINATOR_INSECURE")
      on_exit(fn -> System.delete_env("SYKLI_COORDINATOR_INSECURE") end)
      :ok
    end

    test "allows https to a remote host" do
      assert :ok = HTTP.check_token_transport("https://coord.example.com/v1/runs")
    end

    test "allows plaintext http to loopback (local development)" do
      assert :ok = HTTP.check_token_transport("http://127.0.0.1:4000/v1/runs")
      assert :ok = HTTP.check_token_transport("http://localhost:4000/v1/runs")
    end

    test "refuses a bearer token over plaintext http to a remote host" do
      assert {:error, :insecure_transport} =
               HTTP.check_token_transport("http://coord.example.com/v1/runs")
    end

    test "allows plaintext http to a remote host only with explicit opt-in" do
      assert {:error, :insecure_transport} =
               HTTP.check_token_transport("http://coord.internal/v1/runs")

      System.put_env("SYKLI_COORDINATOR_INSECURE", "1")
      assert :ok = HTTP.check_token_transport("http://coord.internal/v1/runs")
    end
  end

  describe "ssl_opts/1" do
    test "returns verify_peer options for https" do
      opts = HTTP.ssl_opts("https://example.com")
      assert get_in(opts, [:ssl])[:verify] == :verify_peer
    end

    test "returns [] for non-https" do
      assert [] == HTTP.ssl_opts("http://example.com")
    end
  end
end
