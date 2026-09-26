# Changelog

All notable changes to Mado are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
