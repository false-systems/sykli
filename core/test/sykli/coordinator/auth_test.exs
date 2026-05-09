defmodule Sykli.Coordinator.AuthTest do
  use ExUnit.Case, async: true

  import Plug.Test

  alias Sykli.Coordinator.Auth

  test "rejects missing malformed and wrong bearer tokens" do
    assert {:error, :coordinator_unauthorized} =
             Auth.authorize(conn(:get, "/v1/orgs"), token: "secret")

    malformed =
      :get
      |> conn("/v1/orgs")
      |> Plug.Conn.put_req_header("authorization", "Token secret")

    assert {:error, :coordinator_malformed_auth} = Auth.authorize(malformed, token: "secret")

    wrong =
      :get
      |> conn("/v1/orgs")
      |> Plug.Conn.put_req_header("authorization", "Bearer wrong")

    assert {:error, :coordinator_unauthorized} = Auth.authorize(wrong, token: "secret")
  end

  test "accepts correct bearer token" do
    conn =
      :get
      |> conn("/v1/orgs")
      |> Plug.Conn.put_req_header("authorization", "Bearer secret")

    assert :ok = Auth.authorize(conn, token: "secret")
  end

  test "reports missing token configuration" do
    assert {:error, :coordinator_auth_not_configured} = Auth.authorize(conn(:get, "/v1/orgs"))
  end
end
