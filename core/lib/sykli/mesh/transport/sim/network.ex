defmodule Sykli.Mesh.Transport.Sim.Network do
  @moduledoc """
  Simulated network configuration.
  """

  @type t :: %__MODULE__{
          latency: term(),
          packet_loss: term(),
          jitter: term(),
          partitions: term(),
          bandwidth_mbps: term()
        }

  defstruct [:latency, :packet_loss, :jitter, :partitions, :bandwidth_mbps]
end
