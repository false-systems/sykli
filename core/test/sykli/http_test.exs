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

  describe "check_ssrf/1" do
    # IP-literal hosts so resolution is deterministic and DNS-free.
    test "blocks loopback" do
      assert {:error, _} = HTTP.check_ssrf("http://127.0.0.1:8080/hook")
      assert {:error, _} = HTTP.check_ssrf("http://[::1]/hook")
    end

    test "blocks link-local / cloud metadata (169.254.0.0/16)" do
      assert {:error, _} = HTTP.check_ssrf("http://169.254.169.254/latest/meta-data/")
    end

    test "blocks RFC1918 private ranges" do
      assert {:error, _} = HTTP.check_ssrf("http://10.0.0.5/hook")
      assert {:error, _} = HTTP.check_ssrf("http://192.168.1.1/hook")
      assert {:error, _} = HTTP.check_ssrf("http://172.16.0.1/hook")
    end

    test "blocks the full IPv6 link-local (fe80::/10) and ULA (fc00::/7) ranges" do
      # Non-boundary addresses that exact-hextet matching let through.
      assert {:error, _} = HTTP.check_ssrf("http://[fe90::1]/hook")
      assert {:error, _} = HTTP.check_ssrf("http://[febf::1]/hook")
      assert {:error, _} = HTTP.check_ssrf("http://[fc01::1]/hook")
      assert {:error, _} = HTTP.check_ssrf("http://[fd12:3456::1]/hook")
      assert {:error, _} = HTTP.check_ssrf("http://[fdff::1]/hook")
    end

    test "blocks IPv4-mapped IPv6 pointing at private/link-local ranges" do
      assert {:error, _} = HTTP.check_ssrf("http://[::ffff:169.254.169.254]/hook")
      assert {:error, _} = HTTP.check_ssrf("http://[::ffff:10.0.0.1]/hook")
    end

    test "allows public addresses (IPv4 and global IPv6)" do
      assert :ok = HTTP.check_ssrf("https://8.8.8.8/hook")
      assert :ok = HTTP.check_ssrf("https://[2606:4700:4700::1111]/")
    end

    test "rejects a URL with no host" do
      assert {:error, _} = HTTP.check_ssrf("not-a-url")
    end
  end
end
