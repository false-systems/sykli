defmodule Sykli.GitHub.Webhook.Server do
  @moduledoc "Conditional Bandit child for the GitHub webhook receiver."

  require Logger

  @default_port 8617
  @role :webhook_receiver

  def child_spec(opts) do
    %{
      id: __MODULE__,
      start: {__MODULE__, :start_link, [opts]},
      restart: :permanent,
      type: :supervisor
    }
  end

  def start_link(opts \\ []) do
    if enabled?(opts) and Sykli.Mesh.Roles.held_by_local?(@role) do
      port = port(opts)
      Logger.info("[GitHub Webhook] starting receiver", port: port)

      Bandit.start_link(
        plug: {Sykli.GitHub.Webhook.Receiver, opts},
        port: port,
        startup_log: false
      )
    else
      :ignore
    end
  end

  def enabled?(opts \\ []) do
    Keyword.get(opts, :enabled, Application.get_env(:sykli, :github_receiver_enabled, true))
  end

  def port(opts \\ []) do
    Keyword.get(opts, :port, configured_port())
  end

  defp configured_port do
    case System.get_env("SYKLI_GITHUB_RECEIVER_PORT") do
      nil -> Application.get_env(:sykli, :github_receiver_port, @default_port)
      "" -> Application.get_env(:sykli, :github_receiver_port, @default_port)
      value -> String.to_integer(value)
    end
  rescue
    _ -> @default_port
  end
end
