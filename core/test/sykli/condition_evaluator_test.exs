defmodule Sykli.ConditionEvaluatorTest do
  use ExUnit.Case, async: true

  alias Sykli.ConditionEvaluator

  @context %{branch: "release-2.0", tag: "v1.0.0", event: "push", pr_number: nil, ci: true}

  describe "evaluate/2 -- basic operators" do
    test "equality" do
      assert {:ok, true} = ConditionEvaluator.evaluate(~s(branch == "release-2.0"), @context)
      assert {:ok, false} = ConditionEvaluator.evaluate(~s(branch == "main"), @context)
    end

    test "inequality" do
      assert {:ok, true} = ConditionEvaluator.evaluate(~s(branch != "main"), @context)
    end

    test "logical and" do
      assert {:ok, true} =
               ConditionEvaluator.evaluate(~s(ci == true and event == "push"), @context)
    end

    test "logical or" do
      assert {:ok, true} =
               ConditionEvaluator.evaluate(~s(branch == "main" or event == "push"), @context)
    end

    test "not" do
      assert {:ok, false} = ConditionEvaluator.evaluate(~s(not ci), @context)
    end
  end

  describe "evaluate/2 -- regex =~" do
    test "matches regex pattern" do
      assert {:ok, true} = ConditionEvaluator.evaluate(~s(branch =~ "release-.*"), @context)
    end

    test "rejects non-matching regex" do
      assert {:ok, false} = ConditionEvaluator.evaluate(~s(branch =~ "feature-.*"), @context)
    end

    test "handles nil variable" do
      assert {:ok, false} = ConditionEvaluator.evaluate(~s(pr_number =~ ".*"), @context)
    end

    test "complex regex" do
      assert {:ok, true} = ConditionEvaluator.evaluate(~S|tag =~ "v\\d+\\.\\d+\\.\\d+"|, @context)
    end

    test "combined with and/or" do
      assert {:ok, true} =
               ConditionEvaluator.evaluate(
                 ~s(branch =~ "release-.*" and ci == true),
                 @context
               )
    end
  end

  describe "evaluate/2 -- glob matches" do
    test "matches glob pattern" do
      assert {:ok, true} =
               ConditionEvaluator.evaluate(~s[matches(branch, "release-*")], @context)
    end

    test "rejects non-matching glob" do
      assert {:ok, false} =
               ConditionEvaluator.evaluate(~s[matches(branch, "feature/*")], @context)
    end

    test "handles nil variable" do
      assert {:ok, false} =
               ConditionEvaluator.evaluate(~s[matches(pr_number, "*")], @context)
    end

    test "double-star glob matches any depth" do
      ctx = %{branch: "feature/team/thing"}

      assert {:ok, true} =
               ConditionEvaluator.evaluate(~s[matches(branch, "feature/**")], ctx)
    end
  end

  describe "evaluate/2 -- validation errors" do
    test "rejects invalid regex" do
      assert {:error, msg} = ConditionEvaluator.evaluate(~s(branch =~ "[invalid"), @context)
      assert msg =~ "invalid regex"
    end

    test "rejects unknown variables" do
      assert {:error, msg} = ConditionEvaluator.evaluate(~s(foo == "bar"), @context)
      assert msg =~ "unknown variable"
    end
  end
end
