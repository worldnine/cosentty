-- 撮影用の WezTerm 設定。Ghostty の見た目に寄せる(フォント・暗い配色・余白 12px)。
-- タブバーも窓枠も出さず、画面収録で切り取る領域を中身だけにする。
local wezterm = require 'wezterm'
return {
  font = wezterm.font 'Guguru Sans Code',
  font_size = 18.0,
  initial_cols = 106,
  initial_rows = 34,
  window_padding = { left = 12, right = 12, top = 12, bottom = 12 },
  window_decorations = 'NONE',
  enable_tab_bar = false,
  enable_kitty_graphics = true,
  color_scheme = 'Builtin Dark',
  window_close_confirmation = 'NeverPrompt',
  check_for_updates = false,
  default_cursor_style = 'SteadyBlock',
}
