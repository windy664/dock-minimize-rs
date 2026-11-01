// drawer-watch — auto-stash an open Hyprland drawer (special workspace) on an
// outside click, macOS-style.
//
// Two modes:
//
//   drawer-watch click   One-shot check for a mouse-click bind: if a special
//                        workspace is open and the cursor is NOT over one of
//                        its windows, toggle the special workspace closed.
//                        Pair with a non-consuming left-click bind so the
//                        same click still reaches the window underneath.
//
//   drawer-watch watch   Event daemon on Hyprland's event socket: stash the
//                        drawer whenever focus lands outside it. Only useful
//                        when focus follows the mouse loosely (or mouse clicks
//                        don't move focus); with focus-follows-mouse this
//                        stashes on mere hover.
//
// Background: Omarchy's smart-focus dispatcher (hl.dsp.focus) closes an open
// special workspace when focus moves outside it, but with input:special_fallthrough
// disabled a plain mouse click never reaches the windows underneath, so the
// drawer hangs open. The recommended setup is special_fallthrough = true plus
// the "click" bind.

use dock_minimize::{get_hypr_event_socket, get_hypr_socket, hypr_cmd};
use serde_json::Value;
use std::env;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::Duration;

fn open_special_name(cmd_sock: &Path) -> Option<String> {
    let monitors: Value = serde_json::from_str(&hypr_cmd(cmd_sock, "j/monitors")).ok()?;
    monitors.as_array()?.iter().find_map(|m| {
        let name = m
            .get("specialWorkspace")
            .and_then(|sw| sw.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    })
}

fn active_window_workspace(cmd_sock: &Path) -> Option<String> {
    let active: Value = serde_json::from_str(&hypr_cmd(cmd_sock, "j/activewindow")).ok()?;
    active
        .get("workspace")
        .and_then(|w| w.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn log(msg: &str) {
    eprintln!("drawer-watch: {msg}");
}

fn stash(cmd_sock: &Path, special: &str, why: &str) {
    let clean = special.replace("special:", "");
    hypr_cmd(
        cmd_sock,
        &format!(r#"dispatch hl.dsp.workspace.toggle_special("{clean}")"#),
    );
    log(&format!("stash {special} ({why})"));
}

fn cursor_over_drawer_window(cmd_sock: &Path, special: &str) -> bool {
    let cx = hypr_cmd(cmd_sock, "cursorpos");
    let Some((cx, cy)) = cx.split_once(',') else {
        return false;
    };
    let (Ok(cx), Ok(cy)) = (cx.trim().parse::<f64>(), cy.trim().parse::<f64>()) else {
        return false;
    };
    let Ok(clients) = serde_json::from_str::<Value>(&hypr_cmd(cmd_sock, "j/clients")) else {
        return false;
    };
    for c in clients.as_array().unwrap_or(&Vec::new()) {
        let ws = c
            .get("workspace")
            .and_then(|w| w.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if ws != special {
            continue;
        }
        let (Some(at), Some(size)) = (c.get("at").and_then(|v| v.as_array()), c.get("size").and_then(|v| v.as_array())) else {
            continue;
        };
        let (Some(x), Some(y), Some(w), Some(h)) = (
            at.first().and_then(|v| v.as_f64()),
            at.get(1).and_then(|v| v.as_f64()),
            size.first().and_then(|v| v.as_f64()),
            size.get(1).and_then(|v| v.as_f64()),
        ) else {
            continue;
        };
        if cx >= x && cx < x + w && cy >= y && cy < y + h {
            return true;
        }
    }
    false
}

fn click() {
    let Some(cmd_sock) = get_hypr_socket() else {
        log("click: no command socket");
        return;
    };
    let cursor = hypr_cmd(&cmd_sock, "cursorpos");
    let Some(special) = open_special_name(&cmd_sock) else {
        log("click: no drawer open");
        return;
    };
    if cursor_over_drawer_window(&cmd_sock, &special) {
        log("click: cursor over drawer window, keep open");
        return; // clicked inside the drawer — keep it open
    }
    log(&format!("click: stashing {special}, cursor was {cursor:?}"));
    stash(&cmd_sock, &special, "outside click");
}

fn watch() -> std::io::Result<()> {
    let sock = get_hypr_event_socket().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no hyprland event socket")
    })?;
    let stream = UnixStream::connect(sock)?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line?;
        if let Some(addr) = line.strip_prefix("activewindowv2>>") {
            if addr.trim().is_empty() {
                continue;
            }
            let Some(cmd_sock) = get_hypr_socket() else { continue };
            let Some(special) = open_special_name(&cmd_sock) else {
                continue; // no drawer open — nothing to do, stay silent
            };
            let Some(active_ws) = active_window_workspace(&cmd_sock) else {
                eprintln!("drawer-watch: drawer {special} open but no active window");
                continue;
            };
            eprintln!("drawer-watch: focus event, special={special} active_ws={active_ws}");
            if active_ws != special {
                stash(&cmd_sock, &special, "focus moved outside");
            }
        }
    }
    Ok(())
}

fn main() {
    match env::args().nth(1).as_deref() {
        Some("click") => click(),
        _ => loop {
            if let Err(e) = watch() {
                eprintln!("drawer-watch: {e}, retrying in 2s");
            }
            thread::sleep(Duration::from_secs(2));
        },
    }
}
