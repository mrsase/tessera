# Keymap

Application shortcuts are handled before the terminal sees the key. Everything
else (Ctrl+C, Ctrl+D, Ctrl+L, Ctrl+R, Ctrl+Z, Alt+B/F, F-keys, …) is forwarded
to the shell/application, encoded per `tessera-core::input`.

## Tabs

| Chord | Action |
|---|---|
| `Ctrl+Shift+T` | new tab |
| `Ctrl+Shift+W` | close pane; closes the tab when it is the last pane |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | next / previous tab |
| `Ctrl+1` … `Ctrl+9` | select tab by position |
| `F2` | rename tab (palette) |

## Panes

| Chord | Action |
|---|---|
| `Ctrl+Shift+E` | split right (side by side) |
| `Ctrl+Shift+O` | split down (stacked) |
| `Ctrl+Shift+Left/Right/Up/Down` | move focus between panes |
| `Ctrl+Shift+Z` | zoom/unzoom the active pane (UI-only) |
| `Alt+Shift+Left/Right/Up/Down` | nudge the nearest split ratio |

## Terminal

| Chord | Action |
|---|---|
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | copy selection / paste |
| `Ctrl+Shift+A` | select all in the active pane |
| `Shift+PgUp` / `Shift+PgDn` | page through scrollback |
| `Ctrl+Shift+K` | clear scrollback and screen |
| `Ctrl+Shift+F` | search scrollback (palette overlay) |
| `Ctrl+Shift+Plus/Minus/0` | font size up/down/reset |
| `Ctrl+Shift+P` | command palette |
| `Ctrl+,` | open config file path (palette shows it) |

## Suggestions

| Chord | Action |
|---|---|
| `→` at end of line | accept the ghost suggestion |
| `Ctrl+→` | accept one word of the suggestion |
| `Ctrl+Space` | accept the whole suggestion |
| `Esc` | dismiss for the current line |

Ghost text is shown only while the terminal knows the current input line: it is
invalidated by arrows, history recall, tab completion, `Ctrl+C` and entering a
full-screen application.
