defmodule Sykli.SCM do
  @moduledoc """
  SCM facade — auto-detects provider from environment variables and
  delegates commit status updates.

  Detection order: GitHub > GitLab > Bitbucket
  """

  @providers [
    Sykli.SCM.GitHub,
    Sykli.SCM.GitLab,
    Sykli.SCM.Bitbucket
  ]

  @doc "Update commit status using the detected SCM provider."
  @spec update_status(String.t(), String.t(), keyword()) :: :ok | {:error, term()}
  def update_status(task_name, state, opts \\ []) do
    case detect_provider() do
      nil -> :ok
      provider -> provider.update_status(task_name, state, opts)
    end
  end

  @doc "Returns the first enabled SCM provider, or nil."
  @spec detect_provider() :: module() | nil
  def detect_provider do
    Enum.find(@providers, & &1.enabled?())
  end

  @doc "Returns true if any SCM provider is enabled."
  @spec enabled?() :: boolean()
  def enabled? do
    detect_provider() != nil
  end
end
