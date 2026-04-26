defmodule Sykli.CLI.Live do
  @moduledoc "TTY helpers for live `sykli run` rendering."

  @cursor_save "\e7"
  @cursor_restore "\e8"
  @clear_line "\e[2K"

  def tty?(device \\ :stdio) do
    match?({:ok, _}, :io.columns(device))
  end

  def ansi?(device \\ :stdio), do: tty?(device)

  def cursor_save(true), do: @cursor_save
  def cursor_save(false), do: ""

  def cursor_restore(true), do: @cursor_restore
  def cursor_restore(false), do: ""

  def clear_line(true), do: @clear_line
  def clear_line(false), do: ""

  def spinner_frame(tick) when is_integer(tick) do
    frames = Sykli.CLI.Theme.spinner_frames()
    Enum.at(frames, rem(tick, length(frames)))
  end
end
