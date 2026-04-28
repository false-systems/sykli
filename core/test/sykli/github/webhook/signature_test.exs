defmodule Sykli.GitHub.Webhook.SignatureTest do
  use ExUnit.Case, async: true

  alias Sykli.GitHub.Webhook.Signature

  test "validates GitHub SHA-256 signatures" do
    signature = Signature.sign("secret", ~s({"ok":true}))

    assert Signature.valid?("secret", ~s({"ok":true}), signature)
    refute Signature.valid?("wrong", ~s({"ok":true}), signature)
    refute Signature.valid?("secret", ~s({"ok":false}), signature)
    refute Signature.valid?("secret", ~s({"ok":true}), "sha1=bad")
  end
end
