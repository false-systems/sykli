defmodule Sykli.Coordinator.Client do
  @moduledoc """
  Small :httpc-backed client for the self-hosted Team Mode coordinator.
  """

  @timeout_ms 15_000

  def post_json(base_url, path, token, body) do
    request_json(:post, base_url, path, token, body)
  end

  def get_json(base_url, path, token) do
    request_json(:get, base_url, path, token, nil)
  end

  def request_json(method, base_url, path, token, body) do
    url = build_url(base_url, path)
    headers = headers(token)
    request = request_tuple(method, url, headers, body)
    http_opts = [{:timeout, @timeout_ms}] ++ Sykli.HTTP.ssl_opts(url)

    case :httpc.request(method, request, http_opts, []) do
      {:ok, {{_version, status, _reason}, _headers, response_body}} ->
        decode_response(status, response_body)

      {:error, reason} ->
        {:error, {:coordinator_unavailable, reason}}
    end
  end

  defp request_tuple(:get, url, headers, nil), do: {String.to_charlist(url), headers}

  defp request_tuple(:post, url, headers, body) do
    {String.to_charlist(url), headers, ~c"application/json", Jason.encode!(body)}
  end

  defp headers(token) do
    [
      {~c"authorization", ~c"Bearer " ++ String.to_charlist(token)},
      {~c"accept", ~c"application/json"}
    ]
  end

  defp build_url(base_url, path) do
    base = String.trim_trailing(base_url, "/")
    path = "/" <> String.trim_leading(path, "/")
    base <> path
  end

  defp decode_response(status, body) do
    with {:ok, decoded} <- Jason.decode(to_string(body)) do
      case decoded do
        %{"ok" => true, "data" => data} when status in 200..299 -> {:ok, data}
        %{"ok" => false, "error" => error} -> {:error, {:coordinator_error, status, error}}
        _ -> {:error, {:invalid_coordinator_response, decoded}}
      end
    else
      {:error, _reason} -> {:error, :invalid_coordinator_response}
    end
  end
end
