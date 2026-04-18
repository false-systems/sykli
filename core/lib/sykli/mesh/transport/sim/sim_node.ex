defmodule Sykli.Mesh.Transport.Sim.SimNode do
  @moduledoc """
  Simulated node state.
  """

  @type t :: %__MODULE__{
          id: term(),
          profile: term(),
          status: term(),
          capabilities: term(),
          processes: term(),
          inbox: term()
        }

  defstruct [:id, :profile, :status, :capabilities, :processes, :inbox]
end
