<div align="center">

<img src="assets/banner.svg" alt="dock-minimize-rs" width="100%">

</div>

# dock-minimize-rs

![Rust](https://img.shields.io/badge/Rust-2021-d99a6c?style=flat-square&logo=rust&logoColor=white)
![Platform](https://img.shields.io/badge/Omarchy-Hyprland%20%2B%20quickshell-1a79a5?style=flat-square)
![License](https://img.shields.io/badge/License-MIT-3da639?style=flat-square)

Rust tools for the [rosakodu.dock](https://github.com/rosakodu) taskbar
plugin on [Omarchy](https://omarchy.org) (Arch + Hyprland + quickshell):

- **dock-minimize** — a rewrite of the dock's `dock-minimize.py` helper.
  Same behavior, no Python at click time — a single small native binary.
- **drawer-watch** — a tiny daemon that auto-stashes an open drawer
  (special workspace) the moment focus lands on a window outside it,
  like macOS. See [below](#drawer-watch).

## dock-minimize

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

## drawer-watch

Omarchy's smart-focus dispatcher (`hl.dsp.focus`) closes an open special
workspace when focus moves outside it — but a plain *mouse click* never
reaches the windows underneath (`input:special_fallthrough = false` by
default), so an open drawer just hangs there until you toggle it away.

**Recommended setup: click-to-stash.** Enable fallthrough and bind a
non-consuming left-click hook:

```lua
-- ~/.config/hypr/input.lua — let clicks reach the windows under the drawer
hl.config({ input = { special_fallthrough = true } })

-- ~/.config/hypr/bindings.lua — clicking outside an open drawer closes it
hl.bind("mouse:272", hl.dsp.exec_cmd("drawer-watch click"), { non_consuming = true })
```

`drawer-watch click` is a one-shot check: if a special workspace is open and
the cursor is not over one of its windows, it is toggled closed. Clicks on
the drawer's own windows are ignored, and because the bind is non-consuming
plus fallthrough is on, the same click still lands on the window underneath —
one click both dismisses the drawer and activates what you aimed at.

**Fork gotcha:** do *not* add `click = true` on this Hyprland Lua fork.
Click-flag binds fire on button *release*, and with `non_consuming` set the
release callback is regularly dropped, so the stash only works intermittently
(the bind registers as `release` in `hyprctl binds` — that's the tell). A
plain non-consuming bind fires on press and fires every time; verified
against 29/29 real clicks.

There is also `drawer-watch watch`, a daemon that stashes the drawer whenever
focus lands outside it (subscribes to Hyprland's event socket, run it as a
systemd user service if you want that flavor). With focus-follows-mouse this
reacts to *hover*, which is usually not what you want — hence the click bind.

## License

MIT — see [LICENSE](LICENSE). Contains a port of code from rosakodu.dock
(MIT, Copyright rosakodu).
