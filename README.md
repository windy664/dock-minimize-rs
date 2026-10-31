# dock-minimize-rs

A Rust rewrite of the `dock-minimize.py` helper behind the
[rosakodu.dock](https://github.com/rosakodu) taskbar plugin for
[Omarchy](https://omarchy.org) (Arch + Hyprland + quickshell).

Same behavior, no Python at click time — a single small native binary.

## What it does

The dock talks to this binary on every click. It is responsible for all
window-state transitions of dock icons:

- **minimize / restore** — moves windows to and from the `special:minimized`
  workspace (with `hl.dsp.*` dispatches and plain-hyprctl fallbacks)
- **restore-or-launch** — restores the window, or falls back to launching the
  app via `gtk-launch` / desktop `Exec=` when nothing is running
- **CLI app awareness** — a terminal running `yazi`, `nvim`, `btop`, … is
  matched as that CLI app, so the icon acts on the inner app, not the
  terminal; generic terminal icons never swallow dedicated CLI windows
- **Chrome/Chromium/Brave/Edge PWA matching** — window classes like
  `chrome-outlook.office.com__mail_-Profile_1` are matched from the
  `--app=` / `--app-id=` launch command, including profile isolation
- **icon + CLI scanning** — `scan-icons` / `scan-cli` subcommands feed the
  dock's icon resolver and detected-CLI list
- **user drawer support** — a window stashed in a *user* scratchpad special
  workspace (e.g. `special:scratchpad`, not the dock's own
  `special:minimized`) is revealed in place: the drawer workspace is toggled
  open and the window focused, never moved out, so a keybind like
  `Super+S` can stash it right back

## Why Rust

The original is a ~800-line Python script that the dock shells out to on
every single click. This port keeps the logic byte-for-byte compatible
(unit tests are mirrored from the plugin's own `test_dock_minimize.py`)
while removing the Python startup cost and dependency at interaction time.

## Build

```sh
cargo build --release
install -m 755 target/release/dock-minimize ~/.local/bin/dock-minimize
```

## Hook it into the dock

In the plugin, replace the `python3 …/dock-minimize.py` invocations with the
binary. Four call sites:

```js
// DockPanel.qml — two spots (item activate / restore-or-launch)
var binPath = "/home/torin/.local/bin/dock-minimize"
Util.execArgv([binPath].concat(args))

// components/DockWindowTracker.qml — the two scanners
command: ["/home/torin/.local/bin/dock-minimize", "scan-cli"]
command: ["/home/torin/.local/bin/dock-minimize", "scan-icons"]
```

Leave the original `scripts/dock-minimize.py` in place — if a plugin update
reverts the QML, everything still works through the Python path.

## Tests

```sh
cargo test
```

The suite mirrors the upstream Python tests plus the drawer-reveal
regression cases.

## A Hyprland/Omarchy quirk worth knowing

On Omarchy's Hyprland, when a special workspace is open,
`j/activeworkspace` still reports the underlying normal workspace — the open
special workspace only shows up in `j/monitors` → `specialWorkspace`. If you
debug special-workspace logic, watch the monitor field, not the active
workspace.

## License

MIT — see [LICENSE](LICENSE). Contains a port of code from rosakodu.dock
(MIT, Copyright rosakodu).
