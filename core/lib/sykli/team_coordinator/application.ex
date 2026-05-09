defmodule Sykli.TeamCoordinator.Application do
  @moduledoc """
  Supervisor for the self-hosted Team Mode coordinator skeleton.

  This is opt-in and is not started by `Sykli.Application`. The existing
  `Sykli.Coordinator` BEAM mesh process remains separate.
  """

  use Supervisor

  @default_port 8620
  @default_bind "127.0.0.1"

  def start_link(opts \\ []) do
    case Keyword.fetch(opts, :name) do
      {:ok, name} -> Supervisor.start_link(__MODULE__, opts, name: name)
      :error -> Supervisor.start_link(__MODULE__, opts)
    end
  end

  @impl true
  def init(opts) do
    token = Keyword.fetch!(opts, :token)
    port = Keyword.get(opts, :port, @default_port)
    bind = Keyword.get(opts, :bind, @default_bind)
    store_name = Keyword.get(opts, :store_name, Sykli.TeamCoordinator.Store)

    children = [
      {Sykli.TeamCoordinator.Store, store_opts(opts, store_name)},
      {Bandit,
       plug: {Sykli.TeamCoordinator.Router, store: store_name, token: token},
       ip: bind,
       port: port,
       startup_log: false}
    ]

    Supervisor.init(children, strategy: :one_for_one)
  end

  defp store_opts(opts, store_name) do
    opts
    |> Keyword.take([:now, :id])
    |> Keyword.put(:name, store_name)
  end
end
