# Changelog

All notable changes to Mado are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.0.2] - 2026-09-26

### Fixed
- `mado plugin install/update` now restarts Mado correctly whether running as
  the app binary or a CLI symlink — previously traversed to `/usr` and opened
  Finder windows
- Writing the app binary while Mado was running produced a 0-byte executable;
  `bundle.sh` now kills Mado before copying

### Changed
- `bundle.sh` installs to `/Applications/Mado.app` via `ditto` for a clean prod install

## [0.0.1] - 2026-09-25

### Added
- RSS feed plugin support (`mado plugin install news`)
- Single-instance guard (`LSMultipleInstancesProhibited` in Info.plist)
- Auto-restart after `mado plugin install` / `mado plugin update`
- `show = false` projects in projects.toml to hide entries from the sidebar
- Config hot-reload: theme, keybindings, and plugin definitions update live
- Tasku status buttons (mark done, snooze) inline in the task panel
- AI plugin idle notification when Claude finishes a long response
- Deploy/task completion notifications via `notify` MACT action
- `notify` MACT action for system-wide plugin notifications
- Live theme switching via config.toml hot-reload
- projects.toml hot-reload via file watcher
- CLI command list in the help overlay
- Priorities panel drag-to-reorder and sizing fixes
