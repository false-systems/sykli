defmodule Sykli.ContractHash do
  @moduledoc """
  Deterministic hashes for emitted Sykli contracts.

  Team Mode uses canonicalized emitted JSON as the local contract identity.
  This keeps formatting and comments in SDK source files out of the hash and
  avoids including runtime data such as timestamps, durations, or run ids.
  """

  @doc "Computes a sha256-prefixed hash for emitted contract JSON."
  def from_json(json) when is_binary(json) do
    with {:ok, decoded} <- Jason.decode(json) do
      {:ok, from_bytes(Jason.encode!(decoded))}
    else
      {:error, reason} -> {:error, {:contract_hash_failed, :emitted_json, reason}}
    end
  end

  @doc "Computes a sha256-prefixed hash for raw bytes."
  def from_bytes(bytes) when is_binary(bytes) do
    digest =
      :crypto.hash(:sha256, bytes)
      |> Base.encode16(case: :lower)

    "sha256:#{digest}"
  end
end
