defmodule Sykli.HTTP do
  @moduledoc """
  Shared HTTP helpers for :httpc callers.
  Provides TLS verification options for HTTPS endpoints.
  """

  import Bitwise

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

  @doc """
  SSRF guard for outbound webhook URLs.

  Resolves the URL host and rejects loopback, link-local (incl. the cloud
  metadata range `169.254.0.0/16`), and private addresses — for both IPv4 and
  IPv6 — so a pipeline-declared webhook can't be pointed at internal services or
  the instance metadata endpoint. Returns `:ok` or `{:error, reason}`.
  """
  @spec check_ssrf(String.t()) :: :ok | {:error, String.t()}
  def check_ssrf(url) do
    case URI.parse(url).host do
      nil ->
        {:error, "URL has no host"}

      host ->
        host_charlist = String.to_charlist(host)
        v4 = resolve_addrs(host_charlist, :inet)
        v6 = resolve_addrs(host_charlist, :inet6)

        cond do
          v4 == [] and v6 == [] ->
            {:error, "cannot resolve host"}

          # Block if ANY resolved address is private. A host can return multiple
          # A/AAAA records and :httpc re-resolves when it sends, so checking a
          # single sampled address is not enough for an SSRF guard.
          Enum.any?(v4, &private_ip?/1) or Enum.any?(v6, &private_ip6?/1) ->
            {:error, "URL resolves to a private address"}

          true ->
            :ok
        end
    end
  end

  defp resolve_addrs(host_charlist, family) do
    case :inet.getaddrs(host_charlist, family) do
      {:ok, addrs} -> addrs
      {:error, _} -> []
    end
  end

  defp private_ip?({127, _, _, _}), do: true
  defp private_ip?({10, _, _, _}), do: true
  defp private_ip?({172, b, _, _}) when b >= 16 and b <= 31, do: true
  defp private_ip?({192, 168, _, _}), do: true
  defp private_ip?({169, 254, _, _}), do: true
  defp private_ip?({0, 0, 0, 0}), do: true
  defp private_ip?(_), do: false

  defp private_ip6?({0, 0, 0, 0, 0, 0, 0, 0}), do: true
  defp private_ip6?({0, 0, 0, 0, 0, 0, 0, 1}), do: true

  # IPv4-mapped (::ffff:a.b.c.d) — check the embedded IPv4 against the v4 ranges
  # so a mapped private/link-local address can't bypass the guard.
  defp private_ip6?({0, 0, 0, 0, 0, 0xFFFF, ab, cd}) do
    private_ip?({ab >>> 8, ab &&& 0xFF, cd >>> 8, cd &&& 0xFF})
  end

  # Compare the masked first hextet, not exact equality: link-local is fe80::/10
  # (fe80–febf) and unique-local is fc00::/7 (fc00–fdff).
  defp private_ip6?({h, _, _, _, _, _, _, _}) when (h &&& 0xFFC0) == 0xFE80, do: true
  defp private_ip6?({h, _, _, _, _, _, _, _}) when (h &&& 0xFE00) == 0xFC00, do: true
  defp private_ip6?(_), do: false
end
