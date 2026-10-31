//! Shared Hyprland IPC helpers used by both binaries in this crate
//! (`dock-minimize` and `drawer-watch`).

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

fn hypr_runtime_dir() -> PathBuf {
    let runtime = env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    PathBuf::from(runtime).join("hypr")
}

fn find_socket(name: &str) -> Option<PathBuf> {
    let base = hypr_runtime_dir();
    if let Ok(sig) = env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        let p = base.join(&sig).join(name);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(rd) = fs::read_dir(&base) {
        for entry in rd.flatten() {
            let p = entry.path().join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// Path to the Hyprland command socket (`.socket.sock`).
pub fn get_hypr_socket() -> Option<PathBuf> {
    find_socket(".socket.sock")
}

/// Path to the Hyprland event socket (`.socket2.sock`).
pub fn get_hypr_event_socket() -> Option<PathBuf> {
    find_socket(".socket2.sock")
}

/// Send a command to the Hyprland command socket and read the reply to EOF.
/// Returns an empty string on any error.
pub fn hypr_cmd(sock_path: &Path, cmd: &str) -> String {
    let Ok(mut stream) = UnixStream::connect(sock_path) else {
        return String::new();
    };
    if stream.write_all(cmd.as_bytes()).is_err() {
        return String::new();
    }
    let mut res = Vec::new();
    if stream.read_to_end(&mut res).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&res).into_owned()
}
