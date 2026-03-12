defmodule Sykli.ErrorTest do
  use ExUnit.Case, async: true

  alias Sykli.Error

  describe "wrap/1 -- Python errors" do
    test "wraps python_failed into sdk_failed error" do
      error = Error.wrap({:python_failed, "SyntaxError: invalid syntax"})

      assert %Error{} = error
      assert error.code == "sdk_failed"
      assert error.type == :sdk
      assert error.output == "SyntaxError: invalid syntax"
      assert Enum.any?(error.hints, &String.contains?(&1, "python sykli.py"))
    end

    test "wraps python_timeout into sdk_timeout error" do
      error = Error.wrap({:python_timeout, "process timed out"})

      assert %Error{} = error
      assert error.code == "sdk_timeout"
      assert error.type == :sdk
      assert error.duration_ms == 120_000
      assert Enum.any?(error.notes, &(&1 == "process timed out"))
    end
  end

  describe "sdk_failed/2 -- Python hints" do
    test "provides python-specific hint" do
      error = Error.sdk_failed(:python, "ImportError: No module named 'sykli'")

      assert error.code == "sdk_failed"
      assert error.message =~ "Python"
      assert Enum.any?(error.hints, &String.contains?(&1, "python sykli.py"))
    end
  end

  describe "wrap/1 -- passthrough and catch-all" do
    test "passes through existing Error structs" do
      original = Error.internal("test")
      assert Error.wrap(original) == original
    end

    test "wraps unknown atoms" do
      error = Error.wrap(:some_unknown_error)
      assert %Error{} = error
      assert error.type == :internal
    end

    test "wraps unknown binaries" do
      error = Error.wrap("something went wrong")
      assert %Error{} = error
      assert error.message == "something went wrong"
    end
  end
end
