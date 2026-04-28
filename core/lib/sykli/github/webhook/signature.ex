defmodule Sykli.GitHub.Webhook.Signature do
  @moduledoc "GitHub webhook HMAC-SHA256 verification."

  def sign(secret, body) when is_binary(secret) and is_binary(body) do
    digest =
      :crypto.mac(:hmac, :sha256, secret, body)
      |> Base.encode16(case: :lower)

    "sha256=" <> digest
  end

  def valid?(secret, body, "sha256=" <> _ = signature)
      when is_binary(secret) and is_binary(body) do
    expected = sign(secret, body)

    byte_size(expected) == byte_size(signature) and
      Plug.Crypto.secure_compare(expected, signature)
  end

  def valid?(_secret, _body, _signature), do: false
end
