defmodule Sykli.TeamCoordinator.Auth do
  @moduledoc """
  Bearer-token auth for the self-hosted Team Mode coordinator.

  The configured coordinator token is the admin bootstrap token. Team-scoped
  tokens are stateless HMAC-signed claims bound to an org, team, and role.
  """

  import Plug.Conn

  @team_token_prefix "sykli_team_"
  @version "1"
  @roles ~w(owner member approver)

  defmodule Principal do
    @moduledoc false

    @enforce_keys [:type, :role]
    defstruct [:type, :role, :org, :team]
  end

  @doc "Returns the configured coordinator token from opts, app env, or env var."
  def token(opts \\ []) do
    case Keyword.get(opts, :token) || Application.get_env(:sykli, :coordinator_token) ||
           System.get_env("SYKLI_COORDINATOR_TOKEN") do
      value when is_binary(value) and value != "" -> {:ok, value}
      _ -> {:error, :missing_token_config}
    end
  end

  @doc "Checks whether the request carries a valid bearer token."
  def authorize(%Plug.Conn{} = conn, opts \\ []) do
    with {:ok, expected} <- token(opts),
         {:ok, actual} <- bearer_token(conn) do
      cond do
        secure_equal?(actual, expected) ->
          {:ok, %Principal{type: :admin, role: "owner"}}

        true ->
          verify_team_token(actual, expected)
      end
    else
      {:error, :missing_token_config} -> {:error, :coordinator_auth_not_configured}
      {:error, reason} -> {:error, reason}
    end
  end

  def mint_team_token(claims, opts \\ []) when is_map(claims) do
    with {:ok, secret} <- token(opts),
         {:ok, normalized} <- normalize_claims(claims),
         {:ok, claims_json} <- Jason.encode(normalized) do
      encoded_claims = Base.url_encode64(claims_json, padding: false)
      signature = sign(encoded_claims, secret)
      {:ok, @team_token_prefix <> encoded_claims <> "." <> signature}
    end
  end

  def verify_team_token(@team_token_prefix <> token, secret) when is_binary(secret) do
    with [encoded_claims, signature] <- String.split(token, ".", parts: 2),
         true <- valid_signature?(encoded_claims, signature, secret),
         {:ok, claims_json} <- Base.url_decode64(encoded_claims, padding: false),
         {:ok, claims} <- Jason.decode(claims_json),
         {:ok, claims} <- normalize_claims(claims) do
      {:ok,
       %Principal{
         type: :team,
         org: claims["org"],
         team: claims["team"],
         role: claims["role"]
       }}
    else
      _ -> {:error, :coordinator_unauthorized}
    end
  end

  def verify_team_token(_token, _secret), do: {:error, :coordinator_unauthorized}

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

  defp normalize_claims(claims) do
    with {:ok, org} <- required_claim(claims, "org"),
         {:ok, team} <- required_claim(claims, "team"),
         {:ok, role} <- required_claim(claims, "role"),
         :ok <- validate_role(role) do
      {:ok,
       %{
         "version" => Map.get(claims, "version", @version),
         "org" => org,
         "team" => team,
         "role" => role
       }}
    end
  end

  defp required_claim(claims, key) do
    case Map.get(claims, key) || Map.get(claims, String.to_atom(key)) do
      value when is_binary(value) and value != "" -> {:ok, value}
      _ -> {:error, :coordinator_invalid_token_claims}
    end
  end

  defp validate_role(role) when role in @roles, do: :ok
  defp validate_role(_role), do: {:error, :coordinator_invalid_token_claims}

  defp sign(encoded_claims, secret) do
    :hmac
    |> :crypto.mac(:sha256, secret, encoded_claims)
    |> Base.url_encode64(padding: false)
  end

  defp valid_signature?(encoded_claims, signature, secret) when is_binary(signature) do
    expected = sign(encoded_claims, secret)
    secure_equal?(signature, expected)
  end

  defp valid_signature?(_encoded_claims, _signature, _secret), do: false
end
