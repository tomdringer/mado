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

## Projects

### Project paths

`~/.config/mado/projects.toml` maps project codes to root directories. Mado uses this to set the working directory when opening a terminal in a workspace.

```toml
[MDO]
path = "/Users/tom/Sites/mado"

[APP]
path = "/Users/tom/Sites/app"
```

### Task runner

Add a `task` key to any project section to configure a command for the bottom bar runner:

```toml
[MDO]
path = "/Users/tom/Sites/mado"
task = "cargo run"

[APP]
path = "/Users/tom/Sites/app"
task = "npm run dev"

[API]
path = "/Users/tom/Sites/api"
task = "python manage.py runserver"
```

Enable the bottom bar in `~/.config/mado/config.toml`:

```toml
show_bottom_bar = true
```

Click the **RUNNER** strip at the bottom of the window to expand the runner panel. Use the ▶ button to start the task for the current workspace, and ■ to send Ctrl+C. The panel is resizable — drag the top edge to adjust its height.

Projects without a `task` entry show "no task configured for this workspace" in the runner terminal.

---

## Plugins

### What is a plugin?

A Mado sidebar panel plugin is an external binary that Mado spawns and manages. There are two kinds:

| Kind | How it renders |
|------|---------------|
| `terminal` | Writes ANSI escape codes to a PTY — any language, crossterm, ratatui, plain ANSI, shell scripts all work |
| `pixel` | Writes raw RGBA frames to stdout using the Mado frame protocol — full pixel-level control, fonts rendered by the plugin itself |

Pixel plugins are better for custom UIs (timers, clipboards, dashboards). Terminal plugins are better for anything that already has a TUI renderer.

### Built-in panels

These are part of Mado core and are not plugins. They can be toggled on/off in config:

- **Tasku** — task management terminal
- **Priorities** — reorderable project priority list (driven by Tasku data)
- **Workspaces** — project switcher and session parking

### Plugin config

Plugins are declared in `~/.config/mado/config.toml`:

```toml
[[plugins]]
id      = "clock"
command = "/Users/you/.config/mado/plugins/clock"
kind    = "pixel"
icon    = "\uF017"

[[plugins]]
id      = "notes"
command = "mado-notes"
kind    = "terminal"
icon    = "\uF15C"
```

| Field | Description |
|-------|-------------|
| `id` | Unique identifier, shown as the panel title in the sidebar |
| `command` | Binary to spawn — absolute path or on `$PATH` |
| `kind` | `"pixel"` or `"terminal"` |
| `icon` | Nerd Font glyph shown in the sidebar header (e.g. `"\uF017"` = clock) |

### Plugin configuration files

Each plugin can have its own config file at `~/.config/mado/plugins/<id>.toml`. The format is plugin-defined — Mado doesn't read it, the plugin binary loads it directly.

To open a plugin's config in your editor:

```
mado config <plugin-id>
```

For example:

```
mado config clock
mado config pomodoro
```

Plain `mado config` still opens the main `config.toml`.

---

## Pixel plugin protocol

Pixel plugins communicate with Mado over stdin/stdout using a binary frame protocol.

### Frames: plugin → Mado (stdout)

#### Pixel frame

Sends a rendered frame. Mado composites it directly into the sidebar panel.

```
[4 bytes]  magic: b"MADO"
[4 bytes]  width  as u32 little-endian  (physical pixels)
[4 bytes]  height as u32 little-endian  (physical pixels)
[w*h*4 B]  RGBA8 pixel data, row-major, top-to-bottom
```

#### Action message

Sends an action back to Mado. Currently only `"paste"` is supported — it triggers Mado to paste the current clipboard contents into the focused terminal pane.

```
[4 bytes]  magic: b"MACT"
[4 bytes]  JSON length as u32 little-endian
[N bytes]  UTF-8 JSON
```

Example:

```json
{"action": "paste"}
```

### Events: Mado → plugin (stdin)

Events are newline-delimited JSON written to the plugin's stdin.

#### Resize
```json
{"type": "resize", "width": 600, "height": 800}
```
Sent on startup and whenever the panel is resized. Width and height are in physical pixels.

#### Click
```json
{"type": "click", "x": 42.0, "y": 17.0, "button": "left"}
```

#### Key
```json
{"type": "key", "text": "a", "ctrl": false, "meta": false}
```
`text` is the key string (e.g. `"a"`, `"\u{8}"` for backspace, `"\u{1b}"` for escape). `ctrl` and `meta` indicate modifier state.

#### Scroll
```json
{"type": "scroll", "delta": -3.0}
```

#### Focus / Blur
```json
{"type": "focus"}
{"type": "blur"}
```
Sent when the plugin panel gains or loses keyboard focus.

### Minimal Rust pixel plugin

```rust
use std::io::Write;

fn main() {
    let w: u32 = 300;
    let h: u32 = 400;
    let pixels: Vec<u8> = vec![30, 30, 30, 255].repeat((w * h) as usize); // dark grey

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    loop {
        out.write_all(b"MADO").unwrap();
        out.write_all(&w.to_le_bytes()).unwrap();
        out.write_all(&h.to_le_bytes()).unwrap();
        out.write_all(&pixels).unwrap();
        out.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}
```

---

## Terminal plugin protocol

Terminal plugins communicate over a PTY. Mado handles:

- Spawning the process with a PTY sized to the panel
- Forwarding scroll events via `SIGWINCH` and PTY resize
- Rendering the terminal output into the sidebar panel

The plugin reads `COLUMNS` / `LINES` from the environment, renders ANSI to stdout, and handles resize signals normally.

A working terminal plugin can be a shell script:

```sh
#!/bin/sh
while true; do
  clear
  date
  sleep 1
done
```

---

## Reference plugins

| Repo | Kind | Description |
|------|------|-------------|
| **mado-clock** | pixel | Clock, date, and weather with temperature (°C/°F auto-detected) |
| **mado-clipboard** | pixel | Clipboard history manager with search and click-to-paste |
| **mado-pomodoro** | pixel | Pomodoro timer with circular progress, session tracking, and macOS notifications |

### mado-clock config (`~/.config/mado/plugins/clock.toml`)

```toml
# "C" or "F" — leave unset to auto-detect from macOS system preferences
temperature_unit = "F"
```

### mado-pomodoro config (`~/.config/mado/plugins/pomodoro.toml`)

```toml
work_mins                  = 25
short_break_mins           = 5
long_break_mins            = 15
sessions_before_long_break = 4
```

Click anywhere on the timer to start/pause. Click the bottom-right corner to reset.

---

## Plugin discovery

Known community plugins are listed in the **mado-plugins** registry repo. Authors open a PR to add their plugin. Each entry in `registry.json` looks like:

```json
{
  "id": "mado-clock",
  "description": "Clock and weather sidebar panel",
  "repo": "tomdringer/mado-clock",
  "kind": "pixel"
}
```

Install a plugin from the registry:

```
mado plugin install clock
```

Or install directly from a GitHub repo (bypasses registry lookup):

```
mado plugin install tomdringer/mado-clock
```
