defmodule Sykli.Event do
  @moduledoc """
  Mesh and simulation event envelope.
  """

  @type t :: %__MODULE__{
          id: term(),
          at_ms: term(),
          node_id: term(),
          task_id: term(),
          kind: term(),
          what_failed: term(),
          why_it_matters: term(),
          suggested_fix: term(),
          payload: term()
        }

  defstruct [
    :id,
    :at_ms,
    :node_id,
    :task_id,
    :kind,
    :what_failed,
    :why_it_matters,
    :suggested_fix,
    :payload
  ]
end
