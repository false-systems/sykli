defmodule Sykli.Mesh.Transport.Sim.State do
  @moduledoc """
  Simulator transport state.
  """

  @type t :: %__MODULE__{
          nodes: term(),
          clock: term(),
          event_queue: term(),
          seq: term(),
          rng: term(),
          network: term(),
          trace: term(),
          emit_subscribers: term()
        }

  defstruct [
    :nodes,
    :clock,
    :event_queue,
    :seq,
    :rng,
    :network,
    :trace,
    :emit_subscribers
  ]
end
