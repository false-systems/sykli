defmodule Sykli.CLI.Theme do
  @moduledoc "Shared glyphs and colors for Sykli CLI output."

  @accent IO.ANSI.color(80)
  @error IO.ANSI.red()
  @warning IO.ANSI.yellow()
  @dim IO.ANSI.faint()
  @reset IO.ANSI.reset()

  @pass "●"
  @cache "○"
  @fail "✕"
  @blocked "─"
  @running ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]

  def accent, do: @accent
  def error, do: @error
  def warning, do: @warning
  def dim, do: @dim
  def reset, do: @reset

  def glyph(:pass), do: @pass
  def glyph(:cache), do: @cache
  def glyph(:fail), do: @fail
  def glyph(:blocked), do: @blocked
  def glyph(:running), do: hd(@running)

  def spinner_frames, do: @running
end
