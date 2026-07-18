# Changelog — trusty-mpm-gui

All notable changes to trusty-mpm-gui are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Fixed

- Stopped the recurring macOS "'trusty-mpm' would like to access data from other apps" TCC prompt caused by the GUI (closes #2951): renamed `productName`/window title from the bare `"trusty-mpm"` to `"Trusty MPM Dashboard"` so any future prompt is visibly the GUI, not the CLI/daemon; changed the bundle identifier from `com.trusty-mpm.gui` to `com.trusty.trusty-mpm.gui` and wired `bundle.macOS.signingIdentity` in `tauri.conf.json` so `cargo tauri build` produces a Developer-ID-signed `.app` with a stable designated requirement instead of a fresh ad-hoc identity per rebuild.
