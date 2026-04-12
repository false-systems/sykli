defmodule Sykli.Services.NotificationServiceTest do
  use ExUnit.Case, async: false

  alias Sykli.Services.NotificationService

  describe "configured_urls/0" do
    setup do
      saved = System.get_env("SYKLI_WEBHOOK_URLS")

      on_exit(fn ->
        if saved,
          do: System.put_env("SYKLI_WEBHOOK_URLS", saved),
          else: System.delete_env("SYKLI_WEBHOOK_URLS")
      end)

      :ok
    end

    test "returns empty list when env var is nil" do
      System.delete_env("SYKLI_WEBHOOK_URLS")
      assert NotificationService.configured_urls() == []
    end

    test "returns empty list when env var is empty string" do
      System.put_env("SYKLI_WEBHOOK_URLS", "")
      assert NotificationService.configured_urls() == []
    end

    test "parses single URL" do
      System.put_env("SYKLI_WEBHOOK_URLS", "https://example.com/hook")
      assert NotificationService.configured_urls() == ["https://example.com/hook"]
    end

    test "parses comma-separated URLs with trimming" do
      System.put_env(
        "SYKLI_WEBHOOK_URLS",
        "https://a.com/hook , https://b.com/hook , https://c.com/hook"
      )

      assert NotificationService.configured_urls() == [
               "https://a.com/hook",
               "https://b.com/hook",
               "https://c.com/hook"
             ]
    end

    test "filters out empty entries from trailing commas" do
      System.put_env("SYKLI_WEBHOOK_URLS", "https://a.com/hook,,https://b.com/hook,")

      result = NotificationService.configured_urls()
      assert length(result) == 2
      assert "https://a.com/hook" in result
      assert "https://b.com/hook" in result
    end
  end

  describe "notify/1" do
    setup do
      saved = System.get_env("SYKLI_WEBHOOK_URLS")

      on_exit(fn ->
        if saved,
          do: System.put_env("SYKLI_WEBHOOK_URLS", saved),
          else: System.delete_env("SYKLI_WEBHOOK_URLS")
      end)

      :ok
    end

    test "returns :ok even when no URLs configured" do
      System.delete_env("SYKLI_WEBHOOK_URLS")

      assert :ok =
               NotificationService.notify(%{"type" => "ci.run.passed", "run_id" => "test-123"})
    end

    test "returns :ok with SSRF-blocked URL (no network call made)" do
      # Private IP will be rejected by SSRF guard before any network call
      System.put_env("SYKLI_WEBHOOK_URLS", "http://127.0.0.1:8080/hook")

      assert :ok =
               NotificationService.notify(%{"type" => "ci.run.failed", "run_id" => "test-456"})
    end
  end
end
