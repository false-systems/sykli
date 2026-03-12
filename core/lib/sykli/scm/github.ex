defmodule Sykli.SCM.GitHub do
  @moduledoc "GitHub commit status provider. Delegates to existing Sykli.GitHub."
  @behaviour Sykli.SCM.Behaviour

  @impl true
  def enabled?, do: Sykli.GitHub.enabled?()

  @impl true
  def update_status(task_name, state, opts \\ []) do
    Sykli.GitHub.update_status(task_name, state, opts)
  end
end
