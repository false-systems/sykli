defmodule Sykli.SCM.Behaviour do
  @moduledoc """
  Behaviour for SCM (Source Control Management) commit status providers.
  """

  @callback enabled?() :: boolean()
  @callback update_status(task_name :: String.t(), state :: String.t(), opts :: keyword()) ::
              :ok | {:error, term()}
end
