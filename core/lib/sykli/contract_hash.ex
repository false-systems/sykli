defmodule Sykli.ContractHash do
  @moduledoc """
  Deterministic hashes for emitted Sykli contracts.

  Phase 1 Team Mode uses the SDK source file bytes as the local contract
  identity. This keeps the hash stable and avoids including runtime data such
  as timestamps, durations, or run ids.
  """

  @doc "Computes a sha256-prefixed hash for a detected SDK file."
  def from_sdk_file(path) when is_binary(path) do
    case File.read(path) do
      {:ok, bytes} -> {:ok, from_bytes(bytes)}
      {:error, reason} -> {:error, {:contract_hash_failed, path, reason}}
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
