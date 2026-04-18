defmodule Sykli.Simulate.Result do
  @moduledoc """
  Result container for simulation runs.
  """

  @type t :: %__MODULE__{
          seed: term(),
          duration_virtual_ms: term(),
          events: term(),
          timeline: term(),
          executed: term(),
          failed: term(),
          cached: term(),
          skipped: term(),
          retries: term(),
          task_assignments: term(),
          steal_decisions: term(),
          consent_log: term(),
          cleanup_ran: term()
        }

  defstruct [
    :seed,
    :duration_virtual_ms,
    :events,
    :timeline,
    :executed,
    :failed,
    :cached,
    :skipped,
    :retries,
    :task_assignments,
    :steal_decisions,
    :consent_log,
    :cleanup_ran
  ]
end
