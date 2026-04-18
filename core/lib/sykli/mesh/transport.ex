defmodule Sykli.Mesh.Transport do
  @moduledoc """
  Behaviour for mesh transport backends.
  """

  @type node_info :: %{id: String.t(), profile: atom(), status: atom()}

  @callback list_nodes() :: [node_info()]
  @callback spawn_remote(String.t(), {module(), atom(), [term()]}) ::
              {:ok, term()} | {:error, term()}
  @callback send_msg(term(), term()) :: :ok
  @callback monitor(term()) :: reference()
  @callback demonitor(reference()) :: :ok
  @callback now_ms() :: non_neg_integer()
  @callback emit(Sykli.Event.t()) :: :ok
  @callback subscribe(keyword()) :: reference()
end
