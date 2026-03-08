defmodule Sykli.SCM.Bitbucket do
  @moduledoc """
  Bitbucket commit status provider.

  Requires env vars:
  - BITBUCKET_TOKEN: API token (or app password)
  - BITBUCKET_REPO_FULL_NAME: owner/repo
  - BITBUCKET_COMMIT: Commit SHA
  """
  @behaviour Sykli.SCM.Behaviour

  @api_url "https://api.bitbucket.org"

  @impl true
  def enabled? do
    token() != nil and repo() != nil and sha() != nil
  end

  @impl true
  def update_status(task_name, state, opts \\ []) do
    prefix = Keyword.get(opts, :prefix, "ci/sykli")
    key = "#{prefix}/#{task_name}"
    bb_state = map_state(state)

    url = "#{@api_url}/2.0/repositories/#{repo()}/commit/#{sha()}/statuses/build"

    body =
      Jason.encode!(%{
        state: bb_state,
        key: key,
        name: task_name,
        description: description_for(state, task_name)
      })

    url_charlist = String.to_charlist(url)

    headers = [
      {~c"Authorization", String.to_charlist("Bearer #{token()}")},
      {~c"Content-Type", ~c"application/json"}
    ]

    case :httpc.request(
           :post,
           {url_charlist, headers, ~c"application/json", body},
           [{:ssl, ssl_opts()}],
           []
         ) do
      {:ok, {{_, code, _}, _, _}} when code in 200..299 -> :ok
      {:ok, {{_, code, _}, _, resp}} -> {:error, {:http_error, code, to_string(resp)}}
      {:error, reason} -> {:error, reason}
    end
  end

  defp token, do: System.get_env("BITBUCKET_TOKEN")
  defp repo, do: System.get_env("BITBUCKET_REPO_FULL_NAME")
  defp sha, do: System.get_env("BITBUCKET_COMMIT")

  # Bitbucket uses INPROGRESS, SUCCESSFUL, FAILED, STOPPED
  defp map_state("pending"), do: "INPROGRESS"
  defp map_state("success"), do: "SUCCESSFUL"
  defp map_state("failure"), do: "FAILED"
  defp map_state("error"), do: "FAILED"
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
