defmodule Sykli.MCP.Server do
  @moduledoc """
  MCP stdio server — blocking recursive loop with Content-Length framing.

  Reads JSON-RPC messages from stdin, dispatches via `Protocol.handle/1`,
  and writes responses to stdout. Logs to stderr (stdout is the protocol channel).

  ## Framing

  Each message is preceded by `Content-Length: N\\r\\n\\r\\n` where N is the
  byte size of the JSON body. This is the same framing used by LSP.

  ## Usage

      Sykli.MCP.Server.start()
  """

  alias Sykli.MCP.Protocol

  @doc """
  Starts the MCP server loop. Blocks until stdin closes.
  """
  @spec start() :: :ok
  def start do
    log("sykli MCP server starting")
    loop()
    log("sykli MCP server stopped")
    :ok
  end

  @doc """
  Encodes a response map as a Content-Length framed message.
  Returns the raw binary ready to write.
  """
  @spec encode_message(map()) :: binary()
  def encode_message(response) do
    json = Jason.encode!(response)
    byte_size = byte_size(json)
    "Content-Length: #{byte_size}\r\n\r\n#{json}"
  end

  # --- Loop ---

  defp loop do
    case read_message() do
      {:ok, body} ->
        handle_body(body)
        loop()

      :eof ->
        :ok

      {:error, reason} ->
        log("Read error: #{inspect(reason)}")
        :ok
    end
  end

  defp handle_body(body) do
    case Jason.decode(body) do
      {:ok, request} ->
        case Protocol.handle(request) do
          nil ->
            :ok

          response ->
            write_message(response)
        end

      {:error, _} ->
        write_message(Protocol.parse_error())
    end
  end

  # --- IO ---

  defp read_message do
    case read_headers() do
      {:ok, content_length} ->
        case IO.binread(:stdio, content_length) do
          :eof -> :eof
          {:error, _} = err -> err
          data -> {:ok, data}
        end

      :eof ->
        :eof

      {:error, _} = err ->
        err
    end
  end

  defp read_headers do
    read_headers(%{})
  end

  defp read_headers(headers) do
    case IO.binread(:stdio, :line) do
      :eof ->
        :eof

      {:error, _} = err ->
        err

      line ->
        trimmed = String.trim(line)

        if trimmed == "" do
          # Blank line = end of headers (handles both \r\n and \n)
          case headers["content-length"] do
            nil -> {:error, :missing_content_length}
            length -> {:ok, length}
          end
        else
          case String.split(trimmed, ": ", parts: 2) do
            [key, value] ->
              key = String.downcase(key)

              headers =
                if key == "content-length" do
                  case Integer.parse(value) do
                    {n, _} -> Map.put(headers, key, n)
                    :error -> headers
                  end
                else
                  Map.put(headers, key, value)
                end

              read_headers(headers)

            _ ->
              # Skip malformed header lines
              read_headers(headers)
          end
        end
    end
  end

  defp write_message(response) do
    message = encode_message(response)
    IO.binwrite(:stdio, message)
  end

  defp log(message) do
    IO.puts(:stderr, "[MCP] #{message}")
  end
end
