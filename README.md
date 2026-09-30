A terminal multiplexer for macOS and Linux with a sidebar plugin architecture, built in Rust.

![Mado](assets/mado-1200.png)

## Features

- **Pane management** — split terminals horizontally and vertically, tmux-style
- **Sidebar plugins** — left and right sidebars with a growing plugin ecosystem and support for custom plugins
- **22 themes** — built on Tailwind CSS palettes with Starship integration
- **Workspace persistence** — pane layouts and working directories survive project switches
- **Task management** — integrated [Tasku](https://github.com/tomdringer/tasku) panel
- **AI panel** — built-in Claude integration
- **Plugin protocol** — build custom sidebar panels using PTY (ANSI/TUI) or pixel (RGBA frame) protocols

![Themes](assets/mado-themes.png)

> Screenshots use [Starship](https://starship.rs) for the shell prompt. Mado integrates with Starship automatically if it's installed.

## Installation

### macOS: Homebrew

```bash
brew tap tomdringer/tap
brew install --cask tomdringer/tap/mado
```

To update to a new release:

```bash
brew upgrade --cask mado
```

### macOS: manual

Download `Mado.dmg` from the [latest release](https://github.com/tomdringer/mado/releases/latest), open it, and drag Mado to your Applications folder.

### Linux: Debian and Ubuntu

Download the `.deb` for your machine from the [latest release](https://github.com/tomdringer/mado/releases/latest) (`amd64` for Intel/AMD, `arm64` for ARM), then:

```bash
sudo apt install ./mado_*_amd64.deb
```

`apt` installs everything Mado needs, including WebKitGTK for the browser panel. To update, download the new `.deb` and run the same command.

### Linux: other distributions

Download `mado-<version>-linux-x86_64.tar.gz` (or `-aarch64`) from the [latest release](https://github.com/tomdringer/mado/releases/latest), then:

```bash
tar -xzf mado-*-linux-*.tar.gz
cd mado-*-linux-*/
./install.sh
```

This installs Mado into `~/.local` (no sudo) and adds it to your applications menu. Run `./install.sh --remove` from the same folder to uninstall. The browser panel needs WebKitGTK 4.1 from your distribution; `install.sh` tells you if it's missing.

### Linux: Homebrew

```bash
brew install tomdringer/tap/mado
```

Requires Ubuntu 22.04 or newer, or another distribution with glibc 2.35+.

## Building from source

Requires Rust. On macOS, also the Xcode command line tools. On Debian/Ubuntu, also:

```bash
sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libfontconfig1-dev libxkbcommon-dev
```

```bash
git clone https://github.com/tomdringer/mado
cd mado
cargo run
```

## Configuration

Mado is configured via `~/.config/mado/config.toml`. Run `mado config` to open it. Most settings reload live — no restart needed.

### Projects

Define workspaces in `~/.config/mado/projects.toml`:

```toml
default = "API"

[API]
path   = "~/Projects/my-api"
task   = "cargo run"
deploy = "cargo build --release"

[WEB]
path   = "~/Projects/my-frontend"
task   = "npm run dev"
```

If you use [Tasku](https://github.com/tomdringer/tasku), the workspace code must match your Tasku project name for automatic task filtering.

### Themes

Set the active theme in `config.toml`:

```toml
theme = "slate"
```

22 built-in themes are available. Drop a custom `.toml` into `~/.config/mado/themes/` to add your own.

![Tasku panel](assets/mado-tasku.png)

## Plugins

Community plugins are available at [github.com/tomdringer/mado-plugins](https://github.com/tomdringer/mado-plugins), including Clock, Pomodoro, and more. Install a plugin by adding it to your `config.toml`:

```toml
[[plugins]]
id       = "clock"
command  = "/path/to/mado-clock"
kind     = "pixel"
position = "right"
```

See the [Plugins wiki page](https://github.com/tomdringer/mado/wiki/Plugins) for the full protocol specification.

## License

MIT — see [LICENSE](LICENSE).

## Get in touch

For a quick response, please find me on X - @tomdringer
