// drawer-watch — auto-stash an open Hyprland drawer (special workspace) when
// focus lands on a window outside of it.
//
// Stock Hyprland closes an open special workspace when you focus a window
// outside it, but Omarchy's smart-focus dispatch (hl.dsp.focus) is what carries
// that behavior — a plain mouse click focuses a window natively and the drawer
// stays put. This daemon subscribes to Hyprland's event socket: on every
// activewindowv2 event, if a special workspace is open and the newly focused
// window does not live in it, the special workspace is toggled closed.

use dock_minimize::{get_hypr_event_socket, get_hypr_socket, hypr_cmd};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

fn open_special_name(cmd_sock: &std::path::Path) -> Option<String> {
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

fn active_window_workspace(cmd_sock: &std::path::Path) -> Option<String> {
    let active: Value = serde_json::from_str(&hypr_cmd(cmd_sock, "j/activewindow")).ok()?;
    active
        .get("workspace")
        .and_then(|w| w.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn on_focus_change() {
    let Some(cmd_sock) = get_hypr_socket() else { return };
    let Some(special) = open_special_name(&cmd_sock) else { return };
    let Some(active_ws) = active_window_workspace(&cmd_sock) else {
        return;
    };
    if active_ws == special {
        // Focus moved between windows inside the drawer — keep it open.
        return;
    }
    let clean = special.replace("special:", "");
    hypr_cmd(
        &cmd_sock,
        &format!(r#"dispatch hl.dsp.workspace.toggle_special("{clean}")"#),
    );
    eprintln!("drawer-watch: stashed {special} (focus moved to {active_ws})");
}

fn watch() -> std::io::Result<()> {
    let sock = get_hypr_event_socket()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no hyprland event socket"))?;
    let stream = UnixStream::connect(sock)?;
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line?;
        if let Some(addr) = line.strip_prefix("activewindowv2>>") {
            if addr.trim().is_empty() {
                continue;
            }
            on_focus_change();
        }
    }
    Ok(())
}

fn main() {
    loop {
        if let Err(e) = watch() {
            eprintln!("drawer-watch: {e}, retrying in 2s");
        }
        thread::sleep(Duration::from_secs(2));
    }
}
