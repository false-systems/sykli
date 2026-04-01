defmodule Sykli.Services.NotificationService do
  @moduledoc """
  Fire-and-forget webhook notifications for terminal CI events.

  Reads `SYKLI_WEBHOOK_URLS` (comma-separated) and POSTs JSON payloads
  for run pass/fail events. Auto-detects Slack webhook format.

  Timeout: 5s. Never blocks the pipeline.
  """

  require Logger

  @timeout 5_000

  @doc """
  Notify all configured webhooks about a terminal event.
  Fire-and-forget — errors are logged but never propagated.
  """
  @spec notify(map()) :: :ok
  def notify(event) do
    urls = configured_urls()

    if urls != [] do
      # Spawn so we never block the pipeline
      Task.start(fn ->
        Enum.each(urls, fn url ->
          send_notification(url, event)
        end)
      end)
    end

    :ok
  end

  @doc "Returns configured webhook URLs from SYKLI_WEBHOOK_URLS env var."
  @spec configured_urls() :: [String.t()]
  def configured_urls do
    case System.get_env("SYKLI_WEBHOOK_URLS") do
      nil -> []
      "" -> []
      urls -> urls |> String.split(",") |> Enum.map(&String.trim/1) |> Enum.reject(&(&1 == ""))
    end
  end

  defp send_notification(url, event) do
    case validate_url_not_private(url) do
      :ok ->
        do_send_notification(url, event)

      {:error, reason} ->
        Logger.warning("[NotificationService] webhook #{url} rejected: #{reason}")
    end
  end

  defp do_send_notification(url, event) do
    body = format_payload(url, event)
    url_charlist = String.to_charlist(url)

    headers = [{~c"content-type", ~c"application/json"}]
    http_opts = [timeout: @timeout, connect_timeout: @timeout] ++ Sykli.HTTP.ssl_opts(url)

    case :httpc.request(
           :post,
           {url_charlist, headers, ~c"application/json", body},
           http_opts,
           []
         ) do
      {:ok, {{_, code, _}, _, _}} when code in 200..299 ->
        :ok

      {:ok, {{_, code, _}, _, _}} ->
        Logger.warning("[NotificationService] webhook #{url} returned HTTP #{code}")

      {:error, reason} ->
        Logger.warning("[NotificationService] webhook #{url} failed: #{inspect(reason)}")
    end
  end

  # ----- SSRF GUARD -----

  defp validate_url_not_private(url) do
    uri = URI.parse(url)

    case uri.host do
      nil ->
        {:error, "Webhook URL has no host"}

      host ->
        host_charlist = String.to_charlist(host)

        case :inet.getaddr(host_charlist, :inet) do
          {:ok, ip} ->
            if private_ip?(ip) do
              {:error, "Webhook URL resolves to a private address"}
            else
              :ok
            end

          {:error, _} ->
            # Also try IPv6
            case :inet.getaddr(host_charlist, :inet6) do
              {:ok, ip6} ->
                if private_ip6?(ip6) do
                  {:error, "Webhook URL resolves to a private address"}
                else
                  :ok
                end

              {:error, reason} ->
                {:error, "Cannot resolve webhook host: #{inspect(reason)}"}
            end
        end
    end
  end

  defp private_ip?({127, _, _, _}), do: true
  defp private_ip?({10, _, _, _}), do: true
  defp private_ip?({172, b, _, _}) when b >= 16 and b <= 31, do: true
  defp private_ip?({192, 168, _, _}), do: true
  defp private_ip?({169, 254, _, _}), do: true
  defp private_ip?({0, 0, 0, 0}), do: true
  defp private_ip?(_), do: false

  defp private_ip6?({0, 0, 0, 0, 0, 0, 0, 1}), do: true
  defp private_ip6?({0xFE80, _, _, _, _, _, _, _}), do: true
  defp private_ip6?({0xFC00, _, _, _, _, _, _, _}), do: true
  defp private_ip6?({0xFD00, _, _, _, _, _, _, _}), do: true
  defp private_ip6?(_), do: false

  # Auto-detect Slack webhook format
  defp format_payload(url, event) do
    if String.contains?(url, "hooks.slack.com") do
      format_slack(event)
    else
      format_generic(event)
    end
  end

  defp format_slack(event) do
    status = event["type"] || "unknown"
    run_id = event["run_id"] || "?"

    emoji = if String.contains?(status, "passed"), do: ":white_check_mark:", else: ":x:"
    text = "#{emoji} Sykli run `#{run_id}` #{status}"

    Jason.encode!(%{text: text})
  end

  defp format_generic(event) do
    vsn = to_string(Application.spec(:sykli, :vsn) || "unknown")

    source =
      case System.get_env("SYKLI_SOURCE_URI") do
        nil -> Application.get_env(:sykli, :source, "sykli")
        "" -> Application.get_env(:sykli, :source, "sykli")
        uri -> uri
      end

    Jason.encode!(Map.merge(event, %{"source" => source, "version" => vsn}))
  end
end
