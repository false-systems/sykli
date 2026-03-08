defmodule Sykli.Services.SecretMaskerTest do
  use ExUnit.Case, async: true

  alias Sykli.Services.SecretMasker

  describe "mask_string/2" do
    test "replaces known secrets" do
      result = SecretMasker.mask_string("token is abc123xyz", ["abc123xyz"])
      assert result == "token is ***MASKED***"
    end

    test "handles multiple secrets" do
      result = SecretMasker.mask_string("key=secret1 pass=secret2", ["secret1", "secret2"])
      assert result == "key=***MASKED*** pass=***MASKED***"
    end

    test "ignores short secrets (< 4 chars)" do
      result = SecretMasker.mask_string("ab is short", ["ab"])
      assert result == "ab is short"
    end

    test "returns string unchanged when no secrets" do
      assert SecretMasker.mask_string("hello", []) == "hello"
    end

    test "handles non-string input" do
      assert SecretMasker.mask_string(nil, ["secret"]) == nil
      assert SecretMasker.mask_string(42, ["secret"]) == 42
    end
  end

  describe "mask_deep/2" do
    test "masks strings in maps" do
      data = %{"output" => "error: token is mysecret123", "code" => 1}
      result = SecretMasker.mask_deep(data, ["mysecret123"])
      assert result["output"] == "error: token is ***MASKED***"
      assert result["code"] == 1
    end

    test "masks strings in nested maps" do
      data = %{"error" => %{"message" => "failed with key=supersecret"}}
      result = SecretMasker.mask_deep(data, ["supersecret"])
      assert result["error"]["message"] == "failed with key=***MASKED***"
    end

    test "masks strings in lists" do
      data = ["line1: ok", "line2: token=mypassword123"]
      result = SecretMasker.mask_deep(data, ["mypassword123"])
      assert result == ["line1: ok", "line2: token=***MASKED***"]
    end

    test "handles empty secrets list" do
      data = %{"a" => "b"}
      assert SecretMasker.mask_deep(data, []) == data
    end
  end
end
