# Mado Ecosystem: Themes & Plugins

## Themes

### Built-in themes

Mado ships with 22 themes based on the Tailwind CSS color palettes, bundled directly into the binary:

amber, blue, cyan, emerald, fuchsia, gray, green, indigo, lime, neutral, orange, pink, purple, red, rose, sky, slate, stone, teal, violet, yellow, zinc

The default theme is **slate**. To switch, set `theme` in `~/.config/mado/config.toml`:

```toml
theme = "zinc"
```

### Community themes

Community themes live in a single shared repo: **mado-themes**.

- Authors PR a single `.toml` file into that repo
- Themes are reviewed for correctness and merged
- Users install a community theme by dropping the file into `~/.config/mado/themes/`

Mado's theme loader checks `~/.config/mado/themes/<name>.toml` before falling back to the bundled palettes, so user-installed themes always take precedence.

### Theme file format

```toml
[meta]
name = "mytheme"
dark = true

[palette]
"50"  = "#..."
"100" = "#..."
# shades 50–950

[ui]
window_bg        = "#..."
terminal_area_bg = "#..."
pane_bg          = "#..."
pane_toolbar     = "#..."
focus_border     = "#..."
active_dot       = "#..."
card_bg          = "#..."
card_border      = "#..."
divider_active   = "#..."
divider_inactive = "#..."
text_primary     = "#..."
text_muted       = "#..."

[terminal]
foreground = "#..."
background = "#..."

[terminal.ansi]
black          = "#..."
red            = "#..."
green          = "#..."
yellow         = "#..."
blue           = "#..."
magenta        = "#..."
cyan           = "#..."
white          = "#..."
bright_black   = "#..."
bright_red     = "#..."
bright_green   = "#..."
bright_yellow  = "#..."
bright_blue    = "#..."
bright_magenta = "#..."
bright_cyan    = "#..."
bright_white   = "#..."
```

---

## Plugins

### What is a plugin?

A Mado sidebar panel plugin is a binary that Mado spawns and connects to a PTY. Mado handles:

- Spawning the process
- Sizing the PTY to the panel dimensions
- Forwarding scroll events
- Resizing when the user drags the panel handle

The plugin just renders to its terminal. Any language, any rendering approach (crossterm, ratatui, plain ANSI, etc.).

### Built-in panels

These are part of Mado core and are not plugins. They can be toggled on/off in config:

- **Tasku** — task management terminal
- **Priorities** — reorderable project priority list (driven by Tasku data)
- **Workspaces** — project switcher and session parking

These three share data and are tightly coupled to Mado's project model, so extracting them as plugins would require a cross-plugin data API that doesn't exist yet.

### Plugin config

Plugins are declared in `~/.config/mado/config.toml`:

```toml
[[plugins]]
id      = "clock"
command = "mado-clock"

[[plugins]]
id      = "ai"
command = "mado-ai"
```

`id` is used to identify the panel in the sidebar. `command` is the binary Mado will spawn — it must be on `$PATH` or an absolute path.

### Plugin discovery

Known community plugins are listed in `plugins.toml` at the root of the Mado repo. Authors open a PR to add their plugin. Each entry looks like:

```toml
[[plugins]]
name        = "mado-clock"
description = "Clock and weather sidebar panel"
repo        = "https://github.com/example/mado-clock"
kind        = "sidebar-panel"
min_version = "0.1.0"
```

This gives users a curated, searchable list without requiring any registry infrastructure.

### Reference plugins

Two reference implementations are maintained as separate repos to demonstrate the plugin contract:

| Repo | Description | Complexity |
|------|-------------|------------|
| **mado-clock** | Time, date, and weather display | Simple — good starting point |
| **mado-ai** | AI assistant terminal | PTY passthrough, minimal logic |

Start with **mado-clock** if you're building your first plugin. It shows the full shape of a sidebar panel plugin without any PTY complexity beyond rendering.

### Plugin development

A plugin only needs to:

1. Render its UI to stdout using ANSI escape codes
2. Handle `SIGWINCH` to respond to terminal resize events
3. Read `COLUMNS` / `LINES` env vars (or the initial terminal size) for layout

There are no Mado-specific APIs, SDKs, or build tools required. A working plugin can be a shell script.

A `mado-plugin-template` repo will be provided once the plugin contract is stable.
