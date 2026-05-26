defmodule Sykli.TeamCoordinator.AuthTest do
  use ExUnit.Case, async: true

  import Plug.Test

  alias Sykli.TeamCoordinator.Auth

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

    assert {:ok, %{type: :admin, role: "owner"}} = Auth.authorize(conn, token: "secret")
  end

  test "mints and verifies team-scoped tokens" do
    assert {:ok, token} =
             Auth.mint_team_token(
               %{"org" => "false-systems", "team" => "platform", "role" => "approver"},
               token: "secret"
             )

    conn =
      :get
      |> conn("/v1/runs")
      |> Plug.Conn.put_req_header("authorization", "Bearer #{token}")

    assert {:ok, principal} = Auth.authorize(conn, token: "secret")
    assert principal.type == :team
    assert principal.org == "false-systems"
    assert principal.team == "platform"
    assert principal.role == "approver"
  end

  test "rejects tampered team tokens" do
    assert {:ok, token} =
             Auth.mint_team_token(
               %{"org" => "false-systems", "team" => "platform", "role" => "member"},
               token: "secret"
             )

    tampered = token <> "x"

    conn =
      :get
      |> conn("/v1/runs")
      |> Plug.Conn.put_req_header("authorization", "Bearer #{tampered}")

    assert {:error, :coordinator_unauthorized} = Auth.authorize(conn, token: "secret")
  end

  test "reports missing token configuration" do
    assert {:error, :coordinator_auth_not_configured} = Auth.authorize(conn(:get, "/v1/orgs"))
  end
end
