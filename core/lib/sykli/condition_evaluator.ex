defmodule Sykli.ConditionEvaluator do
  @moduledoc """
  Safe condition expression evaluator.

  Only allows:
  - Variable references: branch, tag, event, pr_number, ci
  - String literals: "main", "release"
  - Boolean literals: true, false
  - Comparison: ==, !=
  - Logical: and, or, not
  """

  @allowed_vars [:branch, :tag, :event, :pr_number, :ci, :platform, :runner]

  @doc """
  Evaluates a condition string against a context map.
  Returns true/false or {:error, reason} for invalid conditions.
  """
  def evaluate(condition, context) when is_binary(condition) do
    case Code.string_to_quoted(condition) do
      {:ok, ast} ->
        case validate_ast(ast) do
          :ok -> {:ok, eval_ast(ast, context)}
          {:error, reason} -> {:error, reason}
        end

      {:error, {_line, message, _token}} ->
        {:error, "parse error: #{message}"}
    end
  end

  # ----- AST VALIDATION -----

  # Allow variable references (only whitelisted)
  defp validate_ast({var, _meta, nil}) when var in @allowed_vars, do: :ok

  defp validate_ast({var, _meta, nil}) when is_atom(var) do
    {:error, "unknown variable: #{var}. Allowed: #{Enum.join(@allowed_vars, ", ")}"}
  end

  # Allow string literals
  defp validate_ast(str) when is_binary(str), do: :ok

  # Allow boolean literals
  defp validate_ast(bool) when is_boolean(bool), do: :ok

  # Allow nil
  defp validate_ast(nil), do: :ok

  # Allow comparison operators
  defp validate_ast({op, _meta, [left, right]}) when op in [:==, :!=] do
    with :ok <- validate_ast(left),
         :ok <- validate_ast(right) do
      :ok
    end
  end

  # Allow regex match operator =~
  defp validate_ast({:=~, _meta, [left, right]}) do
    with :ok <- validate_ast(left) do
      # Right side must be a string literal (regex pattern)
      if is_binary(right) do
        case Regex.compile(right) do
          {:ok, _} -> :ok
          {:error, _} -> {:error, "invalid regex pattern: #{right}"}
        end
      else
        {:error, "=~ right-hand side must be a string regex pattern"}
      end
    end
  end

  # Allow matches/2 function call (glob matching)
  defp validate_ast({{:., _meta1, [{:matches, _meta2, nil}]}, _meta3, [left, right]}) do
    with :ok <- validate_ast(left) do
      if is_binary(right), do: :ok, else: {:error, "matches/2 pattern must be a string"}
    end
  end

  # Allow matches(var, pattern) as a direct function call
  defp validate_ast({:matches, _meta, [left, right]}) do
    with :ok <- validate_ast(left) do
      if is_binary(right), do: :ok, else: {:error, "matches/2 pattern must be a string"}
    end
  end

  # Allow logical operators
  defp validate_ast({op, _meta, [left, right]}) when op in [:and, :or] do
    with :ok <- validate_ast(left),
         :ok <- validate_ast(right) do
      :ok
    end
  end

  # Allow 'not' operator
  defp validate_ast({:not, _meta, [expr]}) do
    validate_ast(expr)
  end

  # Reject everything else
  defp validate_ast(other) do
    {:error, "unsupported expression: #{inspect(other)}"}
  end

  # ----- AST EVALUATION -----

  # Variable lookup
  defp eval_ast({var, _meta, nil}, context) when var in @allowed_vars do
    Map.get(context, var)
  end

  # Literals
  defp eval_ast(str, _context) when is_binary(str), do: str
  defp eval_ast(bool, _context) when is_boolean(bool), do: bool
  defp eval_ast(nil, _context), do: nil

  # Comparison
  defp eval_ast({:==, _meta, [left, right]}, context) do
    eval_ast(left, context) == eval_ast(right, context)
  end

  defp eval_ast({:!=, _meta, [left, right]}, context) do
    eval_ast(left, context) != eval_ast(right, context)
  end

  # Logical
  defp eval_ast({:and, _meta, [left, right]}, context) do
    eval_ast(left, context) && eval_ast(right, context)
  end

  defp eval_ast({:or, _meta, [left, right]}, context) do
    eval_ast(left, context) || eval_ast(right, context)
  end

  defp eval_ast({:not, _meta, [expr]}, context) do
    !eval_ast(expr, context)
  end

  # Regex match operator =~
  defp eval_ast({:=~, _meta, [left, right]}, context) when is_binary(right) do
    value = eval_ast(left, context)

    case value do
      nil -> false
      val when is_binary(val) -> Regex.match?(Regex.compile!(right), val)
      _ -> false
    end
  end

  # Glob matching: matches(var, pattern)
  defp eval_ast({:matches, _meta, [left, right]}, context) when is_binary(right) do
    value = eval_ast(left, context)

    case value do
      nil -> false
      val when is_binary(val) -> glob_match?(val, right)
      _ -> false
    end
  end

  # Convert glob pattern to regex
  defp glob_match?(string, pattern) do
    regex_str =
      pattern
      |> Regex.escape()
      |> String.replace("\\*\\*", ".*")
      |> String.replace("\\*", "[^/]*")
      |> String.replace("\\?", ".")

    Regex.match?(Regex.compile!("^" <> regex_str <> "$"), string)
  end
end
