defmodule Sykli.TeamCoordinator.WorkClientTest do
  use ExUnit.Case, async: true

  alias Sykli.TeamCoordinator.WorkClient

  @session %{
    "coordinator" => "https://sykli.internal",
    "org" => "false-systems",
    "team" => "platform",
    "team_id" => "team_001"
  }

  test "create work sends authenticated coordinator request" do
    assert {:ok, %{"id" => "work_001"}} =
             WorkClient.create(
               @session,
               "secret",
               %{"title" => "Investigate deploy", "created_by" => "member:yair"},
               client: __MODULE__.FakeClient
             )

    assert_received {:post, "https://sykli.internal", "/v1/work-items", "secret",
                     %{
                       "org_slug" => "false-systems",
                       "team_slug" => "platform",
                       "title" => "Investigate deploy",
                       "created_by" => "member:yair"
                     }}
  end

  test "list show claim and note use authenticated team endpoints" do
    assert {:ok, [%{"id" => "work_001"}]} =
             WorkClient.list(@session, "secret", client: __MODULE__.FakeClient)

    assert {:ok, %{"id" => "work_001"}} =
             WorkClient.show(@session, "secret", "work_001", client: __MODULE__.FakeClient)

    assert {:ok, %{"status" => "claimed"}} =
             WorkClient.claim(
               @session,
               "secret",
               "work_001",
               %{"assigned_to_type" => "member", "assigned_to_id" => "yair"},
               client: __MODULE__.FakeClient
             )

    assert {:ok, %{"body" => "Found issue"}} =
             WorkClient.note(
               @session,
               "secret",
               "work_001",
               %{"body" => "Found issue"},
               client: __MODULE__.FakeClient
             )

    assert_received {:get, _, "/v1/work-items?team_id=team_001", "secret"}
    assert_received {:get, _, "/v1/work-items/work_001", "secret"}
    assert_received {:post, _, "/v1/work-items/work_001/claim", "secret", _}
    assert_received {:post, _, "/v1/work-items/work_001/notes", "secret", _}
  end

  test "rejects invalid ids before building path" do
    assert {:error, {:invalid_work_item_id, "../escape"}} =
             WorkClient.show(@session, "secret", "../escape", client: __MODULE__.FakeClient)

    refute_received {:get, _, _, _}
  end

  test "maps coordinator transport and response errors" do
    assert {:error, :team_unauthorized} =
             WorkClient.list(@session, "secret", client: __MODULE__.UnauthorizedClient)

    assert {:error, {:team_coordinator_error, %{"code" => "work_item_not_found"}}} =
             WorkClient.show(@session, "secret", "missing", client: __MODULE__.NotFoundClient)

    assert {:error, :team_invalid_coordinator_response} =
             WorkClient.list(@session, "secret", client: __MODULE__.InvalidJsonClient)

    assert {:error, {:team_coordinator_unavailable, :econnrefused}} =
             WorkClient.list(@session, "secret", client: __MODULE__.UnavailableClient)
  end

  defmodule FakeClient do
    def post_json(base, path, token, body) do
      send(self(), {:post, base, path, token, body})

      case path do
        "/v1/work-items" -> {:ok, %{"work_item" => %{"id" => "work_001"}}}
        "/v1/work-items/work_001/claim" -> {:ok, %{"work_item" => %{"status" => "claimed"}}}
        "/v1/work-items/work_001/notes" -> {:ok, %{"note" => %{"body" => body["body"]}}}
      end
    end

    def get_json(base, path, token) do
      send(self(), {:get, base, path, token})

      case path do
        "/v1/work-items?team_id=team_001" -> {:ok, %{"items" => [%{"id" => "work_001"}]}}
        "/v1/work-items/work_001" -> {:ok, %{"work_item" => %{"id" => "work_001"}}}
      end
    end
  end

  defmodule UnauthorizedClient do
    def get_json(_base, _path, _token),
      do: {:error, {:coordinator_error, 401, %{"code" => "coordinator.unauthorized"}}}
  end

  defmodule NotFoundClient do
    def get_json(_base, _path, _token),
      do: {:error, {:coordinator_error, 404, %{"code" => "work_item_not_found"}}}
  end

  defmodule InvalidJsonClient do
    def get_json(_base, _path, _token), do: {:error, :invalid_coordinator_response}
  end

  defmodule UnavailableClient do
    def get_json(_base, _path, _token), do: {:error, {:coordinator_unavailable, :econnrefused}}
  end
end
