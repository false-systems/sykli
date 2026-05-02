defmodule Sykli.GitHub.Source.Real do
  @moduledoc "Git-backed GitHub source acquisition."

  @behaviour Sykli.GitHub.Source.Behaviour

  alias Sykli.Services.SecretMasker

  @base_dir Path.join(System.tmp_dir!(), "sykli-runs")

  @impl true
  def acquire(%{repo: repo, head_sha: sha} = context, token, opts \\ []) do
    run_id =
      Map.get(context, :run_id) || Keyword.get(opts, :run_id) || Map.get(context, :delivery_id)

    root = Path.join(@base_dir, safe_segment(run_id || sha))
    repo_dir = Path.join(root, "repo")

    with :ok <- ensure_contained(root),
         :ok <- File.rm_rf!(root) |> always_ok(),
         :ok <- File.mkdir_p(root),
         :ok <- clone(repo, repo_dir, token),
         :ok <- checkout(repo_dir, sha, token) do
      {:ok, repo_dir}
    else
      {:error, %Sykli.Error{} = error} ->
        cleanup(root, opts)
        {:error, error}

      {:error, reason} ->
        cleanup(root, opts)

        {:error,
         source_error("github.source.clone_failed", "failed to acquire GitHub source", reason)}
    end
  end

  @impl true
  def cleanup(path, opts \\ [])

  def cleanup(path, _opts) when is_binary(path) do
    case run_root(path) do
      {:ok, root} ->
        File.rm_rf!(root)
        :ok

      :error ->
        :ok
    end
  end

  def cleanup(_path, _opts), do: :ok

  defp clone(repo, repo_dir, token) do
    url = auth_url(repo, token)

    case System.cmd("git", ["clone", "--depth", "1", url, repo_dir], stderr_to_stdout: true) do
      {_out, 0} ->
        :ok

      {out, code} ->
        {:error,
         source_error(
           "github.source.clone_failed",
           "git clone failed",
           %{exit_code: code, output: SecretMasker.mask_string(out, [token])}
         )}
    end
  end

  defp checkout(repo_dir, sha, token) do
    case System.cmd("git", ["-C", repo_dir, "checkout", sha], stderr_to_stdout: true) do
      {_out, 0} ->
        :ok

      {_out, _code} ->
        fetch_and_checkout(repo_dir, sha, token)
    end
  end

  defp fetch_and_checkout(repo_dir, sha, token) do
    with {_out, 0} <-
           System.cmd("git", ["-C", repo_dir, "fetch", "--depth", "1", "origin", sha],
             stderr_to_stdout: true
           ),
         {_out, 0} <- System.cmd("git", ["-C", repo_dir, "checkout", sha], stderr_to_stdout: true) do
      :ok
    else
      {out, code} ->
        {:error,
         source_error(
           "github.source.checkout_failed",
           "git checkout failed",
           %{exit_code: code, output: SecretMasker.mask_string(out, [token])}
         )}
    end
  end

  defp auth_url(repo, token),
    do: "https://x-access-token:#{URI.encode_www_form(token)}@github.com/#{repo}.git"

  defp run_root(path) do
    expanded = Path.expand(path)
    base = Path.expand(@base_dir)

    if expanded == base or String.starts_with?(expanded, base <> "/") do
      [_empty, rest] = String.split(expanded, base, parts: 2)
      segment = rest |> String.trim_leading("/") |> String.split("/", parts: 2) |> List.first()

      if segment in [nil, ""] do
        :error
      else
        {:ok, Path.join(base, segment)}
      end
    else
      :error
    end
  end

  defp ensure_contained(path) do
    expanded = Path.expand(path)
    base = Path.expand(@base_dir)

    if String.starts_with?(expanded, base <> "/") do
      :ok
    else
      {:error, source_error("github.source.path_escape", "source path escaped temp directory")}
    end
  end

  defp safe_segment(value) do
    value
    |> to_string()
    |> String.replace(~r/[^A-Za-z0-9._:-]/, "-")
  end

  defp always_ok(_), do: :ok

  defp source_error(code, message, cause \\ nil) do
    %Sykli.Error{
      code: code,
      type: :runtime,
      message: message,
      step: :setup,
      cause: cause,
      hints: []
    }
  end
end
