defmodule Sykli.ReviewPrimitive do
  @moduledoc """
  Deterministic review primitive dispatch.

  Review primitives are not shell commands and do not call LLM providers. They
  evaluate a `kind: "review"` graph node and return structured evidence that can
  be surfaced to agents, gates, humans, and JSON consumers.
  """

  alias Sykli.Graph.Task

  defmodule Result do
    @moduledoc "Structured result returned by a review primitive."

    @enforce_keys [:review_type, :status, :message]
    defstruct [
      :review_type,
      :status,
      :severity,
      :message,
      :tool,
      findings: [],
      evidence: %{}
    ]

    @type status :: :passed | :failed | :unsupported | :errored
    @type t :: %__MODULE__{
            review_type: String.t(),
            status: status(),
            severity: String.t() | nil,
            message: String.t(),
            tool: String.t() | nil,
            findings: [map()],
            evidence: map()
          }
  end

  @api_breakage_primitives ~w(api_breakage api-breakage)

  @doc """
  Evaluate the review primitive for a review node.
  """
  @spec evaluate(Task.t(), map(), keyword()) :: {:ok, Result.t()} | {:error, Result.t()}
  def evaluate(%Task{} = task, state, opts \\ []) do
    primitive = Task.primitive(task)

    cond do
      primitive in @api_breakage_primitives ->
        api_breakage_runner(opts).evaluate(task, state, opts)

      is_binary(primitive) ->
        unsupported(task, primitive, "unsupported review primitive: #{primitive}")

      true ->
        unsupported(task, "unknown", "review node is missing a primitive")
    end
  end

  @doc """
  Return true when a review primitive result should fail the review node.
  """
  @spec failed?(Result.t()) :: boolean()
  def failed?(%Result{status: status}), do: status in [:failed, :unsupported, :errored]

  defp api_breakage_runner(opts) do
    Keyword.get(opts, :review_primitive_runner) ||
      Application.get_env(:sykli, :api_breakage_review_runner, Sykli.ReviewPrimitive.ApiBreakage)
  end

  defp unsupported(task, review_type, message) do
    result = %Result{
      review_type: review_type,
      status: :unsupported,
      severity: "warning",
      message: message,
      evidence: %{
        "task" => task.name,
        "context" => Task.context(task),
        "agent" => Task.agent(task)
      }
    }

    {:error, result}
  end
end

defmodule Sykli.ReviewPrimitive.ApiBreakage do
  @moduledoc """
  Default api_breakage review primitive.

  This module defines the runtime boundary for deterministic API breakage
  checks. Real language/tool adapters are intentionally not bundled here yet;
  until configured, api_breakage returns an explicit unsupported result instead
  of silently passing.
  """

  alias Sykli.Graph.Task
  alias Sykli.ReviewPrimitive.Result

  @spec evaluate(Task.t(), map(), keyword()) :: {:error, Result.t()}
  def evaluate(%Task{} = task, _state, _opts) do
    {:error,
     %Result{
       review_type: "api_breakage",
       status: :unsupported,
       severity: "warning",
       message: "api_breakage review primitive has no configured adapter",
       tool: nil,
       findings: [],
       evidence: %{
         "task" => task.name,
         "context" => Task.context(task),
         "agent" => Task.agent(task)
       }
     }}
  end
end
