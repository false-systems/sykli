defmodule Sykli.Mesh.Transport.Sim.EventQueue do
  @moduledoc """
  Priority queue for simulator events ordered by virtual time and insertion sequence.
  """

  @type entry :: {non_neg_integer(), non_neg_integer(), term()}
  @opaque t :: :gb_sets.set(entry())

  @spec new() :: t()
  def new do
    :gb_sets.empty()
  end

  @spec insert(t(), non_neg_integer(), non_neg_integer(), term()) :: t()
  def insert(set, at_ms, seq, event) do
    :gb_sets.add({at_ms, seq, event}, set)
  end

  @spec pop(t()) :: {:empty, t()} | {:ok, entry(), t()}
  def pop(set) do
    if :gb_sets.is_empty(set) do
      {:empty, set}
    else
      {entry, next_set} = :gb_sets.take_smallest(set)
      {:ok, entry, next_set}
    end
  end

  @spec peek(t()) :: :empty | {:ok, entry()}
  def peek(set) do
    if :gb_sets.is_empty(set) do
      :empty
    else
      {:ok, :gb_sets.smallest(set)}
    end
  end

  @spec size(t()) :: non_neg_integer()
  def size(set), do: :gb_sets.size(set)

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
