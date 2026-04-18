defmodule Sykli.Mesh.Transport.Sim.EventQueue do
  @moduledoc """
  Priority queue for simulator events ordered by virtual time and insertion sequence.
  """

  @type entry :: {non_neg_integer(), non_neg_integer(), term()}
  @opaque t :: {non_neg_integer(), :gb_sets.set(entry())}

  @spec new() :: t()
  def new do
    {0, :gb_sets.empty()}
  end

  @spec insert(t(), non_neg_integer(), non_neg_integer(), term()) :: t()
  def insert({size, set}, at_ms, seq, event) do
    {size + 1, :gb_sets.add({at_ms, seq, event}, set)}
  end

  @spec pop(t()) :: {:empty, t()} | {:ok, entry(), t()}
  def pop({0, _set} = queue), do: {:empty, queue}

  def pop({size, set}) do
    {entry, next_set} = :gb_sets.take_smallest(set)
    {:ok, entry, {size - 1, next_set}}
  end

  @spec peek(t()) :: :empty | {:ok, entry()}
  def peek({_size, set}) do
    try do
      {:ok, :gb_sets.smallest(set)}
    catch
      :error, :function_clause -> :empty
    end
  end

  @spec size(t()) :: non_neg_integer()
  def size({size, _set}), do: size

  @spec drain_until(t(), non_neg_integer()) :: {[entry()], t()}
  def drain_until(queue, until_ms) do
    drain_until(queue, until_ms, [])
  end

  defp drain_until(queue, until_ms, acc) do
    case peek(queue) do
      {:ok, {at_ms, _seq, _event}} when at_ms <= until_ms ->
        {:ok, entry, next_queue} = pop(queue)
        drain_until(next_queue, until_ms, [entry | acc])

      _ ->
        {:lists.reverse(acc), queue}
    end
  end
end
