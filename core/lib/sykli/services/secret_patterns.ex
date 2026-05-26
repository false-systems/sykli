defmodule Sykli.Services.SecretPatterns do
  @moduledoc false

  @min_secret_length 4

  @patterns [
    "_TOKEN",
    "_SECRET",
    "_KEY",
    "_PASSWORD",
    "_PASS",
    "_API_KEY",
    "_CREDENTIAL",
    "_AUTH",
    "_URL",
    "_DSN",
    "_URI",
    "_CONN"
  ]

  @bare_names [
    "TOKEN",
    "SECRET",
    "KEY",
    "PASSWORD",
    "PASS",
    "API_KEY",
    "CREDENTIAL",
    "AUTH"
  ]

  def patterns, do: @patterns

  def secret_key?(key) when is_atom(key), do: key |> Atom.to_string() |> secret_key?()

  def secret_key?(key) when is_binary(key) do
    key = key |> String.upcase() |> String.replace("-", "_")
    key in @bare_names or Enum.any?(@patterns, &String.contains?(key, &1))
  end

  def secret_key?(_key), do: false

  def values_from_env do
    System.get_env()
    |> values_from_pairs()
  end

  def values_from_pairs(nil), do: []

  def values_from_pairs(pairs) when is_map(pairs) or is_list(pairs) do
    pairs
    |> Enum.flat_map(fn
      {key, value} ->
        if secret_key?(key), do: normalize_values(value), else: []

      _other ->
        []
    end)
    |> normalize_values()
  end

  def values_from_pairs(_pairs), do: []

  def all_values(extra_values \\ []) do
    [values_from_env(), extra_values]
    |> List.flatten()
    |> normalize_values()
  end

  def normalize_values(values) when is_list(values) do
    values
    |> Enum.flat_map(&normalize_values/1)
    |> Enum.uniq()
  end

  def normalize_values(value) when is_binary(value) do
    value = String.trim(value)
    if byte_size(value) >= @min_secret_length, do: [value], else: []
  end

  def normalize_values(_value), do: []
end
