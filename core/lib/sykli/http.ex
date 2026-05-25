defmodule Sykli.HTTP do
  @moduledoc """
  Shared HTTP helpers for :httpc callers.
  Provides TLS verification options for HTTPS endpoints.
  """

  @doc """
  Returns SSL options for :httpc that verify server certificates and hostnames.
  """
  @spec ssl_opts(String.t()) :: keyword()
  def ssl_opts(url) do
    uri = URI.parse(url)

    case uri do
      %URI{scheme: "https", host: host} when is_binary(host) and host != "" ->
        [
          ssl: [
            verify: :verify_peer,
            cacerts: :public_key.cacerts_get(),
            server_name_indication: String.to_charlist(host),
            customize_hostname_check: [
              match_fun: :public_key.pkix_verify_hostname_match_fun(:https)
            ],
            depth: 3
          ]
        ]

      _ ->
        []
    end
  end

  @insecure_opt_in_env "SYKLI_COORDINATOR_INSECURE"

  @doc """
  Decides whether `url` is safe to send a bearer token over.

  Returns `:ok` for HTTPS, for loopback hosts (local development), or when the
  `SYKLI_COORDINATOR_INSECURE` opt-in is set; otherwise `{:error, :insecure_transport}`.
  A bearer token sent over plaintext HTTP to a remote host is observable on the
  wire, so callers must refuse rather than leak it.
  """
  @spec check_token_transport(String.t()) :: :ok | {:error, :insecure_transport}
  def check_token_transport(url) do
    cond do
      secure_transport?(url) -> :ok
      loopback_url?(url) -> :ok
      insecure_opt_in?() -> :ok
      true -> {:error, :insecure_transport}
    end
  end

  @doc "True when `url` uses HTTPS."
  @spec secure_transport?(String.t()) :: boolean()
  def secure_transport?(url), do: URI.parse(url).scheme == "https"

  @doc "True when `url`'s host is a loopback address."
  @spec loopback_url?(String.t()) :: boolean()
  def loopback_url?(url) do
    URI.parse(url).host in ["localhost", "127.0.0.1", "::1", "[::1]"]
  end

  @doc "True when the operator has explicitly opted into plaintext token transport."
  @spec insecure_opt_in?() :: boolean()
  def insecure_opt_in?, do: System.get_env(@insecure_opt_in_env) in ["1", "true"]
end
