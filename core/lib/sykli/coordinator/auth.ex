defmodule Sykli.Coordinator.Auth do
  @moduledoc """
  Minimal bearer-token auth for the self-hosted Team Mode coordinator.

  This is intentionally small: Phase 4 only needs an authenticated API
  boundary. Team tokens, OIDC, GitHub org mapping, and RBAC are future layers.
  """

  import Plug.Conn

  @doc "Returns the configured coordinator token from opts, app env, or env var."
  def token(opts \\ []) do
    case Keyword.get(opts, :token) || Application.get_env(:sykli, :coordinator_token) ||
           System.get_env("SYKLI_COORDINATOR_TOKEN") do
      value when is_binary(value) and value != "" -> {:ok, value}
      _ -> {:error, :missing_token_config}
    end
  end

  @doc "Checks whether the request carries the configured bearer token."
  def authorize(%Plug.Conn{} = conn, opts \\ []) do
    with {:ok, expected} <- token(opts),
         {:ok, actual} <- bearer_token(conn),
         true <- secure_equal?(actual, expected) do
      :ok
    else
      {:error, :missing_token_config} -> {:error, :coordinator_auth_not_configured}
      {:error, reason} -> {:error, reason}
      false -> {:error, :coordinator_unauthorized}
    end
  end

  defp bearer_token(conn) do
    case get_req_header(conn, "authorization") do
      ["Bearer " <> token] when token != "" -> {:ok, token}
      [_] -> {:error, :coordinator_malformed_auth}
      [] -> {:error, :coordinator_unauthorized}
    end
  end

  defp secure_equal?(left, right) when byte_size(left) == byte_size(right) do
    Plug.Crypto.secure_compare(left, right)
  end

  defp secure_equal?(_left, _right), do: false
end
