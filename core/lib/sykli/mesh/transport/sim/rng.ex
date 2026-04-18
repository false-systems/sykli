defmodule Sykli.Mesh.Transport.Sim.Rng do
  @moduledoc """
  Seeded RNG wrapper with explicit state threading for simulator determinism.
  """

  @opaque t :: tuple()

  @type seed_tuple :: {pos_integer(), pos_integer(), pos_integer()}

  @spec new(integer()) :: t()
  def new(seed) do
    :rand.seed_s(:exsss, seed_tuple(seed))
  end

  @spec uniform(t(), pos_integer()) :: {non_neg_integer(), t()}
  def uniform(state, max) do
    {value, next_state} = :rand.uniform_s(max, state)
    {value - 1, next_state}
  end

  @spec uniform_real(t()) :: {float(), t()}
  def uniform_real(state) do
    {value, next_state} = :rand.uniform_s(state)
    {1.0 - value, next_state}
  end

  @spec normal(t(), number(), number()) :: {float(), t()}
  def normal(state, mean, stddev) do
    {u1, state} = positive_uniform_real(state)
    {u2, state} = uniform_real(state)

    magnitude = :math.sqrt(-2.0 * :math.log(u1))
    angle = 2.0 * :math.pi() * u2

    {mean + stddev * magnitude * :math.cos(angle), state}
  end

  @spec sample_bool(t(), float()) :: {boolean(), t()}
  def sample_bool(state, probability) do
    {value, next_state} = uniform_real(state)
    {value < probability, next_state}
  end

  @spec positive_uniform_real(t()) :: {float(), t()}
  defp positive_uniform_real(state) do
    :rand.uniform_s(state)
  end

  @spec seed_tuple(integer()) :: seed_tuple()
  defp seed_tuple(seed) do
    {seed_part(seed, 1), seed_part(seed, 2), seed_part(seed, 3)}
  end

  @spec seed_part(integer(), 1 | 2 | 3) :: pos_integer()
  defp seed_part(seed, index) do
    1 + :erlang.phash2({seed, index}, 4_294_967_295)
  end
end
