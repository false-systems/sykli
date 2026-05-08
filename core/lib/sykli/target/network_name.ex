defmodule Sykli.Target.NetworkName do
  @moduledoc false

  def deterministic(task_name, services, state_or_workdir) do
    workdir = workdir_seed(state_or_workdir)

    suffix =
      :crypto.hash(:sha256, :erlang.term_to_binary({task_name, service_seed(services), workdir}))
      |> Base.encode16(case: :lower)
      |> binary_part(0, 12)

    "sykli-#{sanitize_name(task_name)}-#{suffix}"
  end

  defp service_seed(services) do
    Enum.map(services, fn
      %Sykli.Graph.Service{name: name, image: image} ->
        {name, image}

      service when is_map(service) ->
        {service[:name] || service["name"], service[:image] || service["image"]}
    end)
  end

  defp workdir_seed(%{workdir: workdir}), do: workdir
  defp workdir_seed(workdir) when is_binary(workdir), do: workdir
  defp workdir_seed(_state), do: nil

  defp sanitize_name(name) do
    String.replace(name, ~r/[^a-zA-Z0-9_-]/, "_")
  end
end
