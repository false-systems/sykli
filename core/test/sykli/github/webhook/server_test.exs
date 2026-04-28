defmodule Sykli.GitHub.Webhook.ServerTest do
  use ExUnit.Case, async: false

  alias Sykli.GitHub.Webhook.Server
  alias Sykli.Mesh.Roles

  setup do
    Roles.clear()

    on_exit(fn ->
      Roles.clear()
    end)

    :ok
  end

  test "server is a no-op when this node does not hold webhook_receiver" do
    assert :ignore = Server.start_link(enabled: true, port: 0)
  end

  test "server starts when this node holds webhook_receiver" do
    assert :ok = Roles.acquire(:webhook_receiver)
    assert {:ok, pid} = Server.start_link(enabled: true, port: 0, webhook_secret: "secret")
    Process.exit(pid, :normal)
  end
end
