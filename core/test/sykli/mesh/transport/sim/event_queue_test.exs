defmodule Sykli.Mesh.Transport.Sim.EventQueueTest do
  use ExUnit.Case, async: true
  use ExUnitProperties

  alias Sykli.Mesh.Transport.Sim.EventQueue

  test "empty queue returns :empty from pop and peek" do
    queue = EventQueue.new()

    assert :empty == EventQueue.peek(queue)
    assert {:empty, ^queue} = EventQueue.pop(queue)
  end

  test "single event insert then pop returns the event" do
    queue =
      EventQueue.new()
      |> EventQueue.insert(10, 1, :event)

    assert {:ok, {10, 1, :event}} = EventQueue.peek(queue)
    assert {:ok, {10, 1, :event}, empty_queue} = EventQueue.pop(queue)
    assert 0 == EventQueue.size(empty_queue)
  end

  test "events at same at_ms return in seq order" do
    queue =
      EventQueue.new()
      |> EventQueue.insert(10, 3, :third)
      |> EventQueue.insert(10, 1, :first)
      |> EventQueue.insert(10, 2, :second)

    assert [{10, 1, :first}, {10, 2, :second}, {10, 3, :third}] == pop_all(queue)
  end

  test "events at different at_ms return in time order regardless of insertion order" do
    queue =
      EventQueue.new()
      |> EventQueue.insert(30, 3, :late)
      |> EventQueue.insert(10, 2, :early)
      |> EventQueue.insert(20, 1, :middle)

    assert [{10, 2, :early}, {20, 1, :middle}, {30, 3, :late}] == pop_all(queue)
  end

  test "drain_until returns events with at_ms <= until and leaves the rest" do
    queue =
      EventQueue.new()
      |> EventQueue.insert(10, 1, :a)
      |> EventQueue.insert(15, 2, :b)
      |> EventQueue.insert(20, 3, :c)

    assert {[{10, 1, :a}, {15, 2, :b}], rest} = EventQueue.drain_until(queue, 15)
    assert {:ok, {20, 3, :c}} = EventQueue.peek(rest)
    assert 1 == EventQueue.size(rest)
  end

  property "repeated pop returns entries in ascending {at_ms, seq} order" do
    check all(
            seqs <- StreamData.uniq_list_of(StreamData.integer()),
            at_times <-
              StreamData.list_of(StreamData.non_negative_integer(), length: length(seqs)),
            events <- StreamData.list_of(StreamData.term(), length: length(seqs))
          ) do
      triples =
        seqs
        |> Enum.zip(at_times)
        |> Enum.zip(events)
        |> Enum.map(fn {{seq, at_ms}, event} -> %{at_ms: at_ms, seq: seq, event: event} end)

      queue =
        Enum.reduce(triples, EventQueue.new(), fn %{at_ms: at_ms, seq: seq, event: event}, acc ->
          EventQueue.insert(acc, at_ms, seq, event)
        end)

      expected =
        triples
        |> Enum.map(fn %{at_ms: at_ms, seq: seq, event: event} -> {at_ms, seq, event} end)
        |> Enum.sort()

      assert expected == pop_all(queue)
    end
  end

  test "size grows and shrinks as expected" do
    queue = EventQueue.new()
    assert 0 == EventQueue.size(queue)

    queue = EventQueue.insert(queue, 5, 1, :a)
    assert 1 == EventQueue.size(queue)

    queue = EventQueue.insert(queue, 6, 2, :b)
    assert 2 == EventQueue.size(queue)

    assert {:ok, _entry, queue} = EventQueue.pop(queue)
    assert 1 == EventQueue.size(queue)

    assert {:ok, _entry, queue} = EventQueue.pop(queue)
    assert 0 == EventQueue.size(queue)
  end

  defp pop_all(queue) do
    pop_all(queue, [])
  end

  defp pop_all(queue, acc) do
    case EventQueue.pop(queue) do
      {:ok, entry, next_queue} -> pop_all(next_queue, [entry | acc])
      {:empty, _queue} -> Enum.reverse(acc)
    end
  end
end
