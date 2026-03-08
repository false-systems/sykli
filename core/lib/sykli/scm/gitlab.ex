defmodule Sykli.SCM.GitLab do
  @moduledoc """
  GitLab commit status provider.

  Requires env vars:
  - GITLAB_TOKEN: API token
  - CI_PROJECT_ID: GitLab project ID
  - CI_COMMIT_SHA: Commit SHA
  - CI_API_V4_URL: GitLab API URL (default: https://gitlab.com/api/v4)
  """
  @behaviour Sykli.SCM.Behaviour

  @impl true
  def enabled? do
    token() != nil and project_id() != nil and sha() != nil
  end

  @impl true
  def update_status(task_name, state, opts \\ []) do
    prefix = Keyword.get(opts, :prefix, "ci/sykli")
    context = "#{prefix}/#{task_name}"
    gitlab_state = map_state(state)

    api_url = System.get_env("CI_API_V4_URL") || "https://gitlab.com/api/v4"
    url = "#{api_url}/projects/#{project_id()}/statuses/#{sha()}"

    body =
      URI.encode_query(%{
        "state" => gitlab_state,
        "name" => context,
        "description" => description_for(state, task_name)
      })

    url_charlist = String.to_charlist(url)

    headers = [
      {~c"PRIVATE-TOKEN", String.to_charlist(token())},
      {~c"Content-Type", ~c"application/x-www-form-urlencoded"}
    ]

    case :httpc.request(
           :post,
           {url_charlist, headers, ~c"application/x-www-form-urlencoded", body},
           [{:ssl, ssl_opts()}],
           []
         ) do
      {:ok, {{_, code, _}, _, _}} when code in 200..299 -> :ok
      {:ok, {{_, code, _}, _, resp}} -> {:error, {:http_error, code, to_string(resp)}}
      {:error, reason} -> {:error, reason}
    end
  end

  defp token, do: System.get_env("GITLAB_TOKEN")
  defp project_id, do: System.get_env("CI_PROJECT_ID")
  defp sha, do: System.get_env("CI_COMMIT_SHA")

  # GitLab uses different state names
  defp map_state("pending"), do: "running"
  defp map_state("success"), do: "success"
  defp map_state("failure"), do: "failed"
  defp map_state("error"), do: "failed"
  defp map_state(other), do: other

  defp description_for("pending", task), do: "Running #{task}..."
  defp description_for("success", task), do: "#{task} passed"
  defp description_for("failure", task), do: "#{task} failed"
  defp description_for(_, task), do: "#{task}"

  defp ssl_opts do
    [
      verify: :verify_peer,
      cacerts: :public_key.cacerts_get(),
      depth: 3,
      customize_hostname_check: [
        match_fun: :public_key.pkix_verify_hostname_match_fun(:https)
      ]
    ]
  end
end
