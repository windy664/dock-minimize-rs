// show-desktop — Ubuntu/macOS-style "show desktop" toggle for Hyprland.
//
// The compositor has no native minimize-all dispatcher, so this emulates it
// with a special workspace: the first invocation moves every window on the
// active workspace to `special:desktop` and remembers their addresses; the
// second moves them back and restores focus.
//
// Bind it to Super+D:
//   hl.bind("SUPER + D", hl.dsp.exec_cmd("show-desktop"))
//
// State lives in $XDG_RUNTIME_DIR/show-desktop.state (one line: workspace id,
// followed by the stashed window addresses, last entry = the window that had
// focus). Runtime-dir placement means stale state never survives a reboot.

use dock_minimize::{get_hypr_socket, hypr_cmd};
use serde_json::Value;
use std::env;
use std::fs;
use std::path::PathBuf;

const SPECIAL: &str = "special:desktop";

fn state_path() -> PathBuf {
    let dir = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join("show-desktop.state")
}

fn move_window(sock: &std::path::Path, addr: &str, ws: &str) {
    hypr_cmd(
        sock,
        &format!(
            r#"dispatch hl.dsp.window.move({{ window = "address:{addr}", workspace = "{ws}" }})"#
        ),
    );
}

fn active_workspace_id(sock: &std::path::Path) -> Option<i64> {
    let aw: Value = serde_json::from_str(&hypr_cmd(sock, "j/activeworkspace")).ok()?;
    aw.get("id")?.as_i64()
}

fn window_address_at(sock: &std::path::Path, ws_id: i64) -> Option<String> {
    let aw: Value = serde_json::from_str(&hypr_cmd(sock, "j/activewindow")).ok()?;
    let ws = aw.get("workspace")?.get("id")?.as_i64()?;
    if ws != ws_id {
        return None;
    }
    aw.get("address")?.as_str().map(|s| s.to_string())
}

/// Addresses of windows currently on workspace `ws_id`, in stacking order.
fn windows_on_workspace(sock: &std::path::Path, ws_id: i64) -> Vec<String> {
    let clients: Value = match serde_json::from_str(&hypr_cmd(sock, "j/clients")) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    clients
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|c| {
                    c.get("workspace").and_then(|w| w.get("id")).and_then(|v| v.as_i64())
                        == Some(ws_id)
                })
                .filter_map(|c| c.get("address").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn stash(sock: &std::path::Path) {
    let Some(ws_id) = active_workspace_id(sock) else {
        eprintln!("show-desktop: no active workspace");
        return;
    };
    let addrs = windows_on_workspace(sock, ws_id);
    if addrs.is_empty() {
        return; // nothing to hide — desktop already shown
    }
    let last_focus = window_address_at(sock, ws_id);
    let mut lines = vec![ws_id.to_string()];
    lines.extend(addrs.iter().cloned());
    if let Some(f) = last_focus {
        lines.push(format!("focus:{f}"));
    }
    let _ = fs::write(state_path(), lines.join("\n") + "\n");
    for addr in &addrs {
        move_window(sock, addr, SPECIAL);
    }
}

fn restore(sock: &std::path::Path) {
    let Ok(state) = fs::read_to_string(state_path()) else {
        stash(sock); // no state: first press semantics
        return;
    };
    let _ = fs::remove_file(state_path());
    let mut lines = state.lines();
    let Some(ws_id) = lines.next().and_then(|l| l.parse::<i64>().ok()) else {
        return;
    };
    let mut last_focus = None;
    for line in lines {
        if let Some(f) = line.strip_prefix("focus:") {
            last_focus = Some(f.to_string());
            continue;
        }
        // Only windows that are still parked in special:desktop are moved
        // back; the user may have closed or relocated others meanwhile.
        let clients: Value = match serde_json::from_str(&hypr_cmd(sock, "j/clients")) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let parked = clients
            .as_array()
            .map(|arr| {
                arr.iter().any(|c| {
                    c.get("address").and_then(|v| v.as_str()) == Some(line)
                        && c.get("workspace")
                            .and_then(|w| w.get("name"))
                            .and_then(|v| v.as_str())
                            == Some(SPECIAL)
                })
            })
            .unwrap_or(false);
        if parked {
            move_window(sock, line, &ws_id.to_string());
        }
    }
    if let Some(f) = last_focus {
        hypr_cmd(
            sock,
            &format!(r#"dispatch hl.dsp.focus({{ window = "address:{f}" }})"#),
        );
    }
}

fn main() {
    let Some(sock) = get_hypr_socket() else {
        eprintln!("show-desktop: no hyprland socket");
        return;
    };
    if state_path().exists() {
        restore(&sock);
    } else {
        stash(&sock);
    }
}
