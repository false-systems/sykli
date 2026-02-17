defmodule Sykli.MCP.ServerTest do
  use ExUnit.Case, async: true

  alias Sykli.MCP.Server

  describe "encode_message/1" do
    test "produces valid Content-Length framed message" do
      response = %{"jsonrpc" => "2.0", "id" => 1, "result" => %{}}
      message = Server.encode_message(response)

      assert message =~ ~r/^Content-Length: \d+\r\n\r\n/

      # Parse it back
      [header_part, json_part] = String.split(message, "\r\n\r\n", parts: 2)
      assert "Content-Length: " <> length_str = header_part
      content_length = String.to_integer(length_str)

      assert byte_size(json_part) == content_length
      assert {:ok, decoded} = Jason.decode(json_part)
      assert decoded["jsonrpc"] == "2.0"
      assert decoded["id"] == 1
    end

    test "Content-Length is byte size, not string length" do
      # Use a response with unicode to verify byte_size is used
      response = %{"jsonrpc" => "2.0", "id" => 1, "result" => %{"msg" => "hello"}}
      message = Server.encode_message(response)

      [header_part, json_part] = String.split(message, "\r\n\r\n", parts: 2)
      "Content-Length: " <> length_str = header_part
      content_length = String.to_integer(length_str)

      assert byte_size(json_part) == content_length
    end

    test "handles empty result" do
      response = %{"jsonrpc" => "2.0", "id" => 1, "result" => %{}}
      message = Server.encode_message(response)

      [_header, json] = String.split(message, "\r\n\r\n", parts: 2)
      assert Jason.decode!(json) == %{"jsonrpc" => "2.0", "id" => 1, "result" => %{}}
    end
  end
end
