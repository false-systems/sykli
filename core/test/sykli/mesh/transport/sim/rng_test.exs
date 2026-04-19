defmodule Sykli.Mesh.Transport.Sim.RngTest do
  use ExUnit.Case, async: true

  alias Sykli.Mesh.Transport.Sim.Rng

  test "same seed produces same sequence across 1000 calls" do
    assert sequence(Rng.new(1234), 1_000, &Rng.uniform(&1, 1_000_000)) ==
             sequence(Rng.new(1234), 1_000, &Rng.uniform(&1, 1_000_000))
  end

  test "different seeds produce different sequences" do
    for seed <- 1..10 do
      refute sequence(Rng.new(seed), 100, &Rng.uniform(&1, 1_000_000)) ==
               sequence(Rng.new(seed + 10_000), 100, &Rng.uniform(&1, 1_000_000))
    end
  end

  test "uniform stays within [0, max - 1] across 10_000 samples" do
    samples = sequence(Rng.new(42), 10_000, &Rng.uniform(&1, 17))

    assert Enum.all?(samples, fn value -> value >= 0 and value < 17 end)
  end

  test "uniform_real stays within [0.0, 1.0) across 10_000 samples" do
    samples = sequence(Rng.new(42), 10_000, &Rng.uniform_real/1)

    assert Enum.all?(samples, fn value -> value >= 0.0 and value < 1.0 end)
  end

  test "sample_bool with probability 0.3 returns about 30 percent true across 10_000 samples" do
    samples = sequence(Rng.new(123), 10_000, &Rng.sample_bool(&1, 0.3))
    true_ratio = Enum.count(samples, & &1) / length(samples)

    assert true_ratio >= 0.28
    assert true_ratio <= 0.32
  end

  test "normal with mean 100 and stddev 10 matches empirical distribution across 10_000 samples" do
    samples = sequence(Rng.new(999), 10_000, &Rng.normal(&1, 100, 10))
    mean = Enum.sum(samples) / length(samples)

    variance =
      samples
      |> Enum.reduce(0.0, fn sample, acc -> acc + :math.pow(sample - mean, 2) end)
      |> Kernel./(length(samples))

    stddev = :math.sqrt(variance)

    assert mean >= 99.0
    assert mean <= 101.0
    assert stddev >= 9.5
    assert stddev <= 10.5
  end

  defp sequence(state, count, fun) do
    {values, _state} =
      Enum.map_reduce(1..count, state, fn _, acc_state ->
        fun.(acc_state)
      end)

    values
  end
end
