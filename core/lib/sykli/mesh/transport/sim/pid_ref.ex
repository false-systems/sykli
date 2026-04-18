defmodule Sykli.Mesh.Transport.Sim.PidRef do
  @moduledoc """
  Identifier for a virtual process in the simulator.
  """

  @type t :: %__MODULE__{
          node_id: String.t(),
          local_id: non_neg_integer(),
          spawned_at_ms: non_neg_integer()
        }

  defstruct [:node_id, :local_id, :spawned_at_ms]
end
