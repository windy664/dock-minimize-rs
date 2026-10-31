// dock-minimize — Rust port of rosakodu.dock scripts/dock-minimize.py.
//
// Line-for-line behavior migration, including the user-drawer reveal patch:
// a window sitting in a user scratchpad special workspace (e.g.
// special:scratchpad, NOT the dock's special:minimized) is revealed in place
// by toggling that workspace open and focusing the window — it is never
// moved out, so Super+S still stashes it back.

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;

fn normalize(s: &str) -> String {
    let mut val = s.trim().to_lowercase();
    if val.ends_with(".desktop") {
        val.truncate(val.len() - 8);
    }
    if val.ends_with(".exe") {
        val.truncate(val.len() - 4);
    }
    for p in ["org.", "com.", "net.", "io."] {
        val = val.replace(p, "");
    }
    val.chars().filter(|c| c.is_alphanumeric()).collect()
}

const KNOWN_CLI_COMMANDS: &[&str] = &[
    "yazi", "nvim", "neovim", "vim", "nano", "micro", "helix", "hx", "emacs", "kakoune", "kak", "amp",
    "btop", "htop", "top", "bottom", "btm", "glances", "bashtop", "bpytop", "nvtop", "gotop",
    "ranger", "superfile", "broot", "vifm", "nnn", "lf", "fff", "mc", "midnight-commander", "clifm",
    "lazygit", "lazydocker", "tig", "gitui", "k9s", "ox", "bandwhich", "gping",
    "ncmpcpp", "cmus", "mocp", "cava", "cliamp", "rmpc", "spotify-tui", "spt", "mopidy", "musikcube",
    "weechat", "irssi", "profanity", "neomutt", "mutt", "aerc", "gomuks", "senpai",
    "tmux", "zellij", "cmatrix", "pipes.sh", "fastfetch", "neofetch", "cbonsai", "tty-clock", "peaclock", "termshark", "glow", "curseofwar",
];

const IGNORED_COMMAND_PREFIXES: &[&str] = &[
    "sudo", "doas", "pkexec", "pacman", "yay", "paru", "apt", "dnf", "zypper",
    "cargo", "npm", "pnpm", "yarn", "bun", "git", "make", "ninja", "cmake",
    "pip", "python", "python3", "node", "go", "rustc", "gcc", "clang",
    "find", "grep", "cat", "less", "more", "tail", "journalctl", "systemctl",
    "sh", "bash", "zsh", "fish", "exec", "run", "echo", "rm", "cp", "mv",
    "which", "whereis", "man", "info", "curl", "wget", "tar", "unzip", "zip",
    "home", "user", "usr", "etc", "bin", "tmp", "var", "opt", "desktop", "documents", "downloads", "music", "pictures", "videos",
];

const KNOWN_TERMINALS: &[&str] = &[
    "foot", "footclient", "kitty", "alacritty", "org.alacritty",
    "ghostty", "com.mitchellh.ghostty", "wezterm", "wezterm-gui",
    "org.wezfurlong.wezterm", "gnome-terminal", "org.gnome.terminal",
    "konsole", "org.kde.konsole", "xfce4-terminal", "tilix",
    "com.gexperts.tilix", "xterm", "uxterm", "urxvt", "rxvt",
    "rxvt-unicode", "termite", "terminator", "lxterminal", "st",
    "simple-terminal", "rio", "contour", "blackbox",
    "com.raggesilver.blackbox", "ptyxis", "org.gnome.ptyxis",
    "tabby", "hyper", "warp", "warp-terminal",
];

fn canonical_cli(t: &str) -> String {
    match t {
        "neovim" | "vim" => "nvim".to_string(),
        "hx" => "helix".to_string(),
        "btm" => "bottom".to_string(),
        _ => t.to_string(),
    }
}

// Python: re.split(r'[\s:,\-_/\\()\[\]{}|]+', s), empty tokens dropped.
static TOKEN_SPLIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\s:,\-_/\\()\[\]{}|]+").unwrap());

fn tokenize(s: &str) -> Vec<String> {
    TOKEN_SPLIT
        .split(s)
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

fn get_terminal_child_app(pid: i64) -> String {
    if pid <= 0 {
        return String::new();
    }
    // Check command line of terminal (e.g. "foot -e cliamp", "ghostty -e yazi")
    if let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline")) {
        let cmd_raw = String::from_utf8_lossy(&raw).replace('\0', " ").trim().to_string();
        for t in tokenize(&cmd_raw.to_lowercase()) {
            if (KNOWN_CLI_COMMANDS.contains(&t.as_str()) || t == "cliamp") && !is_terminal_identifier(&t) {
                return canonical_cli(&t);
            }
        }
    }

    // Check child processes
    if let Ok(content) = fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
        for cpid in content.split_whitespace() {
            if let Ok(comm) = fs::read_to_string(format!("/proc/{cpid}/comm")) {
                let comm = comm.trim().to_lowercase();
                if KNOWN_CLI_COMMANDS.contains(&comm.as_str()) || comm == "cliamp" {
                    return canonical_cli(&comm);
                }
            }
            if let Ok(raw) = fs::read(format!("/proc/{cpid}/cmdline")) {
                let cmd_raw = String::from_utf8_lossy(&raw).replace('\0', " ").trim().to_string();
                for t in tokenize(&cmd_raw.to_lowercase()) {
                    if (KNOWN_CLI_COMMANDS.contains(&t.as_str()) || t == "cliamp") && !is_terminal_identifier(&t) {
                        return canonical_cli(&t);
                    }
                }
            }
            // Check grandchild processes in case child was a wrapper shell
            if let Ok(gc) = fs::read_to_string(format!("/proc/{cpid}/task/{cpid}/children")) {
                for gcpid in gc.split_whitespace() {
                    if let Ok(comm) = fs::read_to_string(format!("/proc/{gcpid}/comm")) {
                        let comm = comm.trim().to_lowercase();
                        if KNOWN_CLI_COMMANDS.contains(&comm.as_str()) || comm == "cliamp" {
                            return canonical_cli(&comm);
                        }
                    }
                }
            }
        }
    }
    String::new()
}

fn extract_cli_app(title: &str, pid: Option<i64>) -> String {
    if let Some(p) = pid {
        let proc_app = get_terminal_child_app(p);
        if !proc_app.is_empty() {
            return proc_app;
        }
    }

    if title.is_empty() {
        return String::new();
    }
    let raw = title.trim().to_lowercase();
    let tokens = tokenize(&raw);
    if tokens.is_empty() {
        return String::new();
    }

    // 1. Check all tokens against known commands
    for tok in &tokens {
        if IGNORED_COMMAND_PREFIXES.contains(&tok.as_str()) {
            continue;
        }
        if KNOWN_CLI_COMMANDS.contains(&tok.as_str()) || tok == "cliamp" {
            return canonical_cli(tok);
        }
    }

    // 2. Check special phrases
    if raw.contains("nvim") && tokens.iter().any(|t| t == "nvim") {
        return "nvim".to_string();
    }
    if raw.contains("whips") || raw.contains("terminal's ass") || raw.contains("cliamp") {
        return "cliamp".to_string();
    }

    String::new()
}

fn is_terminal_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut raw = s.trim().to_lowercase();
    if raw.ends_with(".desktop") {
        raw.truncate(raw.len() - 8);
    }
    // normalize() strips only the org./com./net./io. prefix, so a reverse-DNS
    // app-id such as "com.mitchellh.ghostty" normalizes to "mitchellhghostty"
    // and never equals "ghostty". Also test the last dotted segment.
    let mut candidates = vec![normalize(s)];
    if raw.contains('.') {
        let last = raw.rsplit('.').next().unwrap_or("");
        candidates.push(normalize(last));
    }
    for term in KNOWN_TERMINALS {
        let nt = normalize(term);
        for cand in &candidates {
            if !cand.is_empty() && *cand == nt {
                return true;
            }
        }
    }
    false
}

fn get_hypr_socket() -> Option<PathBuf> {
    let runtime = env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));
    if let Ok(sig) = env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        let p = PathBuf::from(&runtime).join("hypr").join(&sig).join(".socket.sock");
        if p.exists() {
            return Some(p);
        }
    }
    let base = PathBuf::from(&runtime).join("hypr");
    if let Ok(rd) = fs::read_dir(&base) {
        for entry in rd.flatten() {
            let p = entry.path().join(".socket.sock");
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

fn hypr_cmd(sock_path: &Path, cmd: &str) -> String {
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

fn sfield(c: &Value, key: &str) -> String {
    c.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn ws_name(c: &Value) -> String {
    c.get("workspace")
        .and_then(|w| w.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn raise_window(sock_path: &Path, c: &Value) {
    // Restack a window above the others so a focused float is not buried.
    //
    // hl.dsp.focus changes the active window without touching stacking
    // order, so a floating window covered by another float stays covered
    // after a dock click. bring_to_top does the restack. Only floating
    // windows can be buried; tiled targets are skipped to keep layout
    // behavior identical.
    if !c.get("floating").and_then(|v| v.as_bool()).unwrap_or(false) {
        return;
    }
    let addr = sfield(c, "address").to_lowercase();
    if addr.is_empty() {
        return;
    }
    let res = hypr_cmd(
        sock_path,
        &format!(
            r#"dispatch hl.dsp.window.alter_zorder({{ window = "address:{addr}", mode = "top" }})"#
        ),
    );
    if res.to_lowercase().contains("error") {
        hypr_cmd(
            sock_path,
            &format!(r#"dispatch hl.dsp.window.bring_to_top({{ window = "address:{addr}" }})"#),
        );
    }
}

fn close_any_special(sock_path: &Path) {
    if let Ok(monitors) = serde_json::from_str::<Value>(&hypr_cmd(sock_path, "j/monitors")) {
        if let Some(arr) = monitors.as_array() {
            for m in arr {
                let sw_name = m
                    .get("specialWorkspace")
                    .and_then(|sw| sw.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !sw_name.is_empty() {
                    let clean_name = sw_name.replace("special:", "");
                    hypr_cmd(
                        sock_path,
                        &format!(r#"dispatch hl.dsp.workspace.toggle_special("{clean_name}")"#),
                    );
                }
            }
        }
    }
}

fn reveal_user_drawer(sock_path: &Path, src_ws: &str, addr: &str) {
    // 窗口在用户自建抽屉（special:scratchpad 等，非 dock 的 special:minimized）时：
    // 不把窗口搬出抽屉，只展开对应抽屉并聚焦窗口；按 Super+S 可收回。
    let name = src_ws.replace("special:", "");
    let mut focused_ws_name = String::new();
    if let Ok(focused) = serde_json::from_str::<Value>(&hypr_cmd(sock_path, "j/activeworkspace")) {
        if let Some(n) = focused.get("name").and_then(|v| v.as_str()) {
            focused_ws_name = n.to_string();
        }
    }
    if focused_ws_name != src_ws {
        hypr_cmd(
            sock_path,
            &format!(r#"dispatch hl.dsp.workspace.toggle_special("{name}")"#),
        );
    }
    hypr_cmd(
        sock_path,
        &format!(r#"dispatch hl.dsp.focus({{ window = "address:{addr}" }})"#),
    );
}

static PCT_PCT: LazyLock<Regex> = LazyLock::new(|| Regex::new("%%").unwrap());
static PCT_FIELD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"%[a-zA-Z]").unwrap());

fn parse_desktop_exec_argv(exec_str: &str) -> Vec<String> {
    if exec_str.is_empty() {
        return Vec::new();
    }
    // Remove FreeDesktop field codes (%f, %F, %u, %U, %i, %c, %k, %v, %m, %d, %D, %n, %N, %v)
    let raw = PCT_PCT.replace_all(exec_str, "\u{0}").into_owned();
    let raw = PCT_FIELD.replace_all(&raw, "").into_owned();
    let raw = raw.replace('\u{0}', "%");
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    match shlex::split(raw) {
        Some(v) if !v.is_empty() => v,
        _ => raw.split_whitespace().map(|s| s.to_string()).collect(),
    }
}

fn find_desktop_file(desktop_id: &str) -> String {
    if desktop_id.is_empty() || desktop_id.contains('/') || desktop_id.contains('\0') {
        return String::new();
    }
    // A ".desktop" tail can belong to the application's own name: the entry id
    // org.telegram.desktop names the file org.telegram.desktop.desktop.
    let mut names = vec![format!("{desktop_id}.desktop")];
    if desktop_id.ends_with(".desktop") {
        names.push(desktop_id.to_string());
    }
    let home = env::var("HOME").unwrap_or_default();
    let mut search_dirs = vec![
        PathBuf::from(&home).join(".local/share/applications"),
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        PathBuf::from(&home).join(".local/share/flatpak/exports/share/applications"),
    ];
    if let Ok(xdg) = env::var("XDG_DATA_DIRS") {
        for d in xdg.split(':') {
            if d.is_empty() {
                continue;
            }
            let app_d = PathBuf::from(d).join("applications");
            if !search_dirs.contains(&app_d) {
                search_dirs.push(app_d);
            }
        }
    }

    for base in &search_dirs {
        for name in &names {
            if base.join(name).is_file() {
                return name.clone();
            }
        }
    }
    String::new()
}

fn split_queries(queries: &[String]) -> (Vec<String>, i64, String) {
    // Separate the app identifiers from the window selector.
    //
    // A dock click sends the app's identifiers (app id, desktop id, exec) and,
    // optionally, which of that app's windows to act on: a Hyprland address
    // ("0x..."), or the legacy "--index=N" / bare-N position. A selector picks a
    // window out of the app's windows -- it must not narrow the match itself, or
    // the app's other windows stop existing as far as the caller can tell, and
    // logic like "focus a sibling after minimizing" has nothing left to find.
    //
    // Only a "0x" prefix counts as an address. Hyprland always writes them that
    // way, and app identifiers that happen to be spelled out of hex digits
    // ("deface") are identifiers.
    let mut target_index: i64 = -1;
    let mut target_addr = String::new();
    let mut identifiers = Vec::new();
    for q in queries {
        if let Some(rest) = q.strip_prefix("--index=") {
            if let Ok(n) = rest.parse::<i64>() {
                target_index = n;
            }
        } else if !q.is_empty() && q.chars().all(|c| c.is_ascii_digit()) && target_index == -1 {
            if let Ok(n) = q.parse::<i64>() {
                target_index = n;
            }
        } else if q.len() >= 2 && q[..2].eq_ignore_ascii_case("0x") && target_addr.is_empty() {
            target_addr = q.to_lowercase();
        } else {
            identifiers.push(q.clone());
        }
    }
    (identifiers, target_index, target_addr)
}

fn parse_url_host_path(raw_url: &str) -> (String, String) {
    // Mimics urllib.parse.urlparse for the absolute http(s) URLs seen in
    // Chrome --app= launch commands: scheme://netloc/path?query#fragment.
    let rest = match raw_url.find("://") {
        Some(i) => &raw_url[i + 3..],
        None => raw_url,
    };
    let netloc_end = rest
        .find(|c| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let host = rest[..netloc_end].to_lowercase();
    let after = &rest[netloc_end..];
    let path = if after.starts_with('/') {
        let end = after.find(|c| c == '?' || c == '#').unwrap_or(after.len());
        after[..end].to_string()
    } else {
        "/".to_string()
    };
    (host, path)
}

static OPT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"--app=(?:"([^"]+)"|'([^']+)'|([^\s]+))"#).unwrap());
static PROFILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"--profile-directory=(?:"([^"]+)"|'([^']+)'|([^\s]+))"#).unwrap());
static APP_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"--app-id=(?:"([^"]+)"|'([^']+)'|([^\s]+))"#).unwrap());

fn opt_group(caps: &regex::Captures) -> String {
    caps.get(1)
        .or_else(|| caps.get(2))
        .or_else(|| caps.get(3))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

fn profile_from_query(query: &str) -> Option<String> {
    PROFILE_RE
        .captures(query)
        .map(|c| opt_group(&c).replace(' ', "_").to_lowercase())
}

fn match_chrome_pwa(c_class: &str, query: &str) -> bool {
    // Match a Hyprland window class against a Chrome/Chromium/Brave/Edge PWA launch query.
    //
    // Handles Chrome web app classes like 'chrome-outlook.office.com__mail_-Profile_1'
    // generated from '--app=https://outlook.office.com/mail/' and '--profile-directory="Profile 1"'.
    if c_class.is_empty() || query.is_empty() {
        return false;
    }

    let c_class = c_class.to_lowercase();
    if !["chrome-", "chromium-", "brave-", "edge-", "msedge-", "microsoft-edge-"]
        .iter()
        .any(|p| c_class.starts_with(p))
    {
        return false;
    }

    // 1. URL-based PWA (--app=...)
    if let Some(caps) = OPT_RE.captures(query) {
        let raw_url = opt_group(&caps);
        let (host, path) = parse_url_host_path(&raw_url);
        let path_replaced = path.replace('/', "_");
        let pwa_key = format!("{host}_{path_replaced}").to_lowercase();
        let alt_key = format!(
            "{host}_{}",
            path.trim_end_matches('/').replace('/', "_")
        )
        .to_lowercase();

        if c_class.contains(&pwa_key) || c_class.contains(&alt_key) {
            match profile_from_query(query) {
                Some(profile) => return c_class.contains(&format!("-{profile}")),
                None => return true,
            }
        }
    }

    // 2. App-ID based PWA (--app-id=...)
    if let Some(caps) = APP_ID_RE.captures(query) {
        let app_id = opt_group(&caps).to_lowercase();
        if !app_id.is_empty() && c_class.contains(&app_id) {
            match profile_from_query(query) {
                Some(profile) => return c_class.contains(&format!("-{profile}")),
                None => return true,
            }
        }
    }

    false
}

fn addr_of(c: &Value) -> String {
    sfield(c, "address").to_lowercase()
}

fn pick_target<'a>(
    matching: &'a [Value],
    visible_windows: &'a [Value],
    minimized_windows: &'a [Value],
    target_addr: &str,
    target_index: i64,
    active_addr: &str,
    all_clients: Option<&'a [Value]>,
) -> Option<&'a Value> {
    // The one window a dock click is aimed at, out of the app's windows.
    //
    // An address names a specific window and wins outright; an index is the
    // legacy form of the same thing and is resolved against Hyprland's client
    // order, which reorders on its own. With neither -- or with a selector whose
    // window is gone -- fall back to the active window (when the caller cares),
    // then the first visible one, then a minimized one.
    if !target_addr.is_empty() {
        for c in matching {
            if addr_of(c) == target_addr {
                return Some(c);
            }
        }
        if let Some(all) = all_clients {
            for c in all {
                if addr_of(c) == target_addr {
                    return Some(c);
                }
            }
        }
    }
    if target_index >= 0 && (target_index as usize) < matching.len() {
        return Some(&matching[target_index as usize]);
    }
    if !active_addr.is_empty() {
        for c in matching {
            if addr_of(c) == active_addr {
                return Some(c);
            }
        }
    }
    if !visible_windows.is_empty() {
        return Some(&visible_windows[0]);
    }
    if !minimized_windows.is_empty() {
        return Some(&minimized_windows[0]);
    }
    matching.first()
}

fn spawn_detached(cmd: &[&str]) -> bool {
    match Command::new(cmd[0])
        .args(&cmd[1..])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => true,
        Err(_) => false,
    }
}

fn which(exe: &str) -> bool {
    if exe.contains('/') {
        return Path::new(exe).is_file();
    }
    let path = env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let p = Path::new(dir).join(exe);
        if let Ok(md) = fs::metadata(&p) {
            if md.is_file() && md.permissions().mode() & 0o111 != 0 {
                return true;
            }
        }
    }
    false
}

fn launch_fallback(queries: &[String]) {
    // 0. Dedicated native Wayland App-ID launch for cliamp / CLI audio player
    for q in queries {
        if q.is_empty() || q.starts_with("0x") || q.starts_with("--") {
            continue;
        }
        let clean_q = q.replace(".desktop", "").to_lowercase().trim().to_string();
        if clean_q == "cliamp" || clean_q == "org.omarchy.cliamp" {
            if spawn_detached(&["foot", "-a", "cliamp", "-T", "cliamp", "-e", "cliamp"]) {
                return;
            }
        }
    }

    // 1. First priority: desktop entry files that actually exist on disk via gtk-launch
    for q in queries {
        if q.is_empty() || q.starts_with("0x") || q.starts_with("--") {
            continue;
        }
        let desktop_id = find_desktop_file(q);
        if !desktop_id.is_empty() {
            if spawn_detached(&["uwsm-app", "--", "gtk-launch", &desktop_id]) {
                return;
            }
            if spawn_detached(&["gtk-launch", &desktop_id]) {
                return;
            }
        }
    }

    // 2. Second priority: executable argv lists without a shell
    for q in queries {
        if q.is_empty() || q.starts_with("0x") || q.starts_with("--") {
            continue;
        }
        let argv = parse_desktop_exec_argv(q);
        if !argv.is_empty() && which(&argv[0]) {
            let mut full: Vec<&str> = vec!["uwsm-app", "--"];
            full.extend(argv.iter().map(|s| s.as_str()));
            if spawn_detached(&full) {
                return;
            }
            let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
            if spawn_detached(&argv_refs) {
                return;
            }
        }
    }
}

static ICON_SUBDIRS: &[&str] = &[
    "hicolor/scalable/apps",
    "hicolor/scalable/devices",
    "hicolor/256x256/apps",
    "hicolor/128x128/apps",
    "hicolor/48x48/apps",
    "hicolor/32x32/apps",
    "hicolor/16x16/apps",
    "scalable/apps",
    "scalable/devices",
];

fn scan_icon_dir(dir: &Path, svg: &mut HashMap<String, String>, png: &mut HashMap<String, String>) {
    if let Ok(rd) = fs::read_dir(dir) {
        for f in rd.flatten() {
            let fl = f.file_name().to_string_lossy().to_lowercase();
            let p = f.path();
            if let Some(stem) = fl.strip_suffix(".svg") {
                svg.entry(stem.to_string()).or_insert_with(|| p.to_string_lossy().into_owned());
            } else if let Some(stem) = fl.strip_suffix(".png") {
                png.entry(stem.to_string()).or_insert_with(|| p.to_string_lossy().into_owned());
            }
        }
    }
}

fn scan_disk_icons() -> HashMap<String, String> {
    // Scan system and user icon directories for app icons.
    //
    // Returns a map of lowercase icon base name -> absolute file path.
    // Scalable SVG icons take precedence over PNGs.
    let home = env::var("HOME").unwrap_or_default();
    let mut base_dirs = vec![
        PathBuf::from(&home).join(".local/share/icons"),
        PathBuf::from(&home).join(".icons"),
        PathBuf::from("/usr/local/share/icons"),
        PathBuf::from("/usr/share/icons"),
    ];
    if let Ok(xdg) = env::var("XDG_DATA_DIRS") {
        for d in xdg.split(':') {
            if d.is_empty() {
                continue;
            }
            let ic = PathBuf::from(d).join("icons");
            if ic.is_dir() && !base_dirs.contains(&ic) {
                base_dirs.push(ic);
            }
        }
    }

    let mut svg_icons: HashMap<String, String> = HashMap::new();
    let mut png_icons: HashMap<String, String> = HashMap::new();

    for b in &base_dirs {
        for sub in ICON_SUBDIRS {
            let p = b.join(sub);
            if p.is_dir() {
                scan_icon_dir(&p, &mut svg_icons, &mut png_icons);
            }
        }
    }

    for p in ["/usr/share/pixmaps", "/usr/local/share/pixmaps"] {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            scan_icon_dir(&pb, &mut svg_icons, &mut png_icons);
        }
    }

    let mut merged = png_icons;
    for (k, v) in svg_icons {
        merged.insert(k, v);
    }

    let mut extras: HashMap<String, String> = HashMap::new();
    for (k, v) in &merged {
        if let Some(suffix) = k.rsplit('.').next() {
            if !suffix.is_empty() && !merged.contains_key(suffix) && !extras.contains_key(suffix) {
                extras.insert(suffix.to_string(), v.clone());
            }
        }
    }
    for (k, v) in extras {
        merged.insert(k, v);
    }
    merged
}

const MODES: &[&str] = &[
    "minimize",
    "restore",
    "restore-or-launch",
    "toggle-active",
    "toggle-or-cycle",
    "toggle-instance",
    "activate-instance",
    "toggle",
    "activate",
    "scan-cli",
    "scan-icons",
];

fn focused_workspace_name(sock_path: &Path) -> String {
    let mut focused_ws = "1".to_string();
    if let Ok(monitors) = serde_json::from_str::<Value>(&hypr_cmd(sock_path, "j/monitors")) {
        if let Some(arr) = monitors.as_array() {
            for m in arr {
                if m.get("focused").and_then(|v| v.as_bool()).unwrap_or(false) {
                    if let Some(ws) = m.get("activeWorkspace") {
                        let name = ws.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let id = match ws.get("id") {
                            Some(Value::Number(n)) => n.to_string(),
                            Some(Value::String(s)) => s.clone(),
                            _ => String::new(),
                        };
                        focused_ws = if !name.is_empty() {
                            name.to_string()
                        } else if !id.is_empty() {
                            id
                        } else {
                            "1".to_string()
                        };
                    }
                    break;
                }
            }
        }
    }
    focused_ws
}

fn restore_window(sock_path: &Path, addr: &str, target_c: &Value, focused_ws: &str) {
    let mut target_ws = if focused_ws.is_empty() { "1" } else { focused_ws }.to_string();
    if target_ws.starts_with("special:") {
        target_ws = "1".to_string();
    }

    let res = hypr_cmd(
        sock_path,
        &format!(
            r#"dispatch hl.dsp.window.move({{ window = "address:{addr}", workspace = "{target_ws}" }})"#
        ),
    );
    if res.to_lowercase().contains("error") {
        hypr_cmd(
            sock_path,
            &format!("dispatch movetoworkspacesilent {target_ws},address:{addr}"),
        );
    }

    let res_focus = hypr_cmd(
        sock_path,
        &format!(r#"dispatch hl.dsp.focus({{ window = "address:{addr}" }})"#),
    );
    if res_focus.to_lowercase().contains("error") {
        hypr_cmd(sock_path, &format!("dispatch focuswindow address:{addr}"));
    }

    raise_window(sock_path, target_c);
    close_any_special(sock_path);
}

fn focus_window(sock_path: &Path, addr: &str) {
    let res_focus = hypr_cmd(
        sock_path,
        &format!(r#"dispatch hl.dsp.focus({{ window = "address:{addr}" }})"#),
    );
    if res_focus.to_lowercase().contains("error") {
        hypr_cmd(sock_path, &format!("dispatch focuswindow address:{addr}"));
    }
}

fn run(args: &[String]) {
    if args.len() < 2 {
        return;
    }

    let mut mode = "minimize";
    let mut arg_start = 1usize;
    if MODES.contains(&args[1].as_str()) {
        mode = args[1].as_str();
        arg_start = 2;
    }

    if mode == "scan-icons" {
        println!("{}", serde_json::to_string(&scan_disk_icons()).unwrap_or_default());
        return;
    }

    let queries: Vec<String> = args[arg_start..]
        .iter()
        .map(|q| q.trim().to_string())
        .filter(|q| !q.is_empty())
        .collect();
    if queries.is_empty() && mode != "scan-cli" {
        return;
    }

    let Some(sock_path) = get_hypr_socket() else {
        if mode == "restore-or-launch" {
            launch_fallback(&queries);
        }
        return;
    };

    let clients_raw = hypr_cmd(&sock_path, "j/clients");
    if clients_raw.is_empty() {
        if mode == "restore-or-launch" {
            launch_fallback(&queries);
        }
        return;
    }

    let clients: Value = match serde_json::from_str(&clients_raw) {
        Ok(v) => v,
        Err(_) => {
            if mode == "restore-or-launch" {
                launch_fallback(&queries);
            }
            return;
        }
    };
    let empty: Vec<Value> = Vec::new();
    let clients = clients.as_array().unwrap_or(&empty);

    if mode == "scan-cli" {
        let mut detected_apps: Vec<String> = Vec::new();
        for c in clients {
            let c_cls = sfield(c, "class").to_lowercase();
            if is_terminal_identifier(&c_cls) {
                let pid = c.get("pid").and_then(|v| v.as_i64());
                let title = sfield(c, "title");
                let app = extract_cli_app(&title, pid);
                if !app.is_empty() && !detected_apps.contains(&app) {
                    detected_apps.push(app);
                }
            }
        }
        println!("{}", serde_json::to_string(&detected_apps).unwrap_or_default());
        return;
    }

    let (queries, target_index, target_addr) = split_queries(&queries);

    let norm_queries: Vec<String> = queries.iter().map(|q| normalize(q)).collect();
    let raw_queries: Vec<String> = queries.iter().map(|q| q.to_lowercase()).collect();

    let mut is_terminal_query = !queries.is_empty() && is_terminal_identifier(&queries[0]);
    let mut target_cli = String::new();
    'cli_search: for q in &queries {
        let norm_q = normalize(q);
        let clean_q = q.replace(".desktop", "").to_lowercase().trim().to_string();
        if KNOWN_CLI_COMMANDS.contains(&clean_q.as_str()) || clean_q == "cliamp" {
            target_cli = canonical_cli(&clean_q);
            break;
        }
        if KNOWN_CLI_COMMANDS.contains(&norm_q.as_str()) {
            target_cli = canonical_cli(&norm_q);
            break;
        }
        for et in tokenize(&q.to_lowercase()) {
            if (KNOWN_CLI_COMMANDS.contains(&et.as_str()) || et == "cliamp") && !is_terminal_identifier(&et) {
                target_cli = canonical_cli(&et);
                break 'cli_search;
            }
        }
    }

    if !target_cli.is_empty() {
        is_terminal_query = false;
    }

    let mut matching: Vec<Value> = Vec::new();
    for c in clients {
        let c_class = sfield(c, "class").to_lowercase();
        let c_init_class = sfield(c, "initialClass").to_lowercase();
        let c_title = sfield(c, "title").to_lowercase();

        let c_norm_class = normalize(&c_class);
        let c_norm_init = normalize(&c_init_class);
        let c_pid = c.get("pid").and_then(|v| v.as_i64());
        let is_client_terminal =
            is_terminal_identifier(&c_class) || is_terminal_identifier(&c_init_class);
        let cli_app = if is_client_terminal {
            extract_cli_app(&c_title, c_pid)
        } else {
            String::new()
        };

        if !target_cli.is_empty() {
            // When clicking a dedicated CLI app icon (e.g. yazi, nvim), match if the
            // terminal window is running that CLI app
            if cli_app == target_cli {
                matching.push(c.clone());
                continue;
            } else if is_client_terminal {
                continue;
            }
        }

        if is_terminal_query {
            // When clicking a generic terminal icon (e.g. foot), never swallow a
            // dedicated CLI app window (e.g. yazi, nvim)
            if !cli_app.is_empty() {
                continue;
            }
        }

        let mut is_match = false;
        for (i, q) in raw_queries.iter().enumerate() {
            let nq = &norm_queries[i];
            if *q == c_class || *q == c_init_class {
                is_match = true;
                break;
            }
            if !nq.is_empty() && (*nq == c_norm_class || *nq == c_norm_init) {
                is_match = true;
                break;
            }
            if !nq.is_empty() && nq.len() >= 3 && (*nq == c_norm_class || *nq == c_norm_init) {
                is_match = true;
                break;
            }
            if match_chrome_pwa(&c_class, &queries[i]) {
                is_match = true;
                break;
            }
        }

        if is_match {
            matching.push(c.clone());
        }
    }

    if !target_addr.is_empty() {
        if let Some(tc) = clients
            .iter()
            .find(|c| addr_of(c) == target_addr)
        {
            if !matching.contains(tc) {
                matching.insert(0, tc.clone());
            }
        }
    }

    let focused_ws = focused_workspace_name(&sock_path);

    let mut visible_windows: Vec<Value> = Vec::new();
    let mut minimized_windows: Vec<Value> = Vec::new();
    for c in &matching {
        if ws_name(c).starts_with("special:") {
            minimized_windows.push(c.clone());
        } else {
            visible_windows.push(c.clone());
        }
    }

    if [
        "toggle-instance",
        "toggle",
        "toggle-active",
        "minimize",
        "minimize-instance",
    ]
    .contains(&mode)
    {
        if matching.is_empty() {
            return;
        }

        let mut active_addr = String::new();
        let active_raw = hypr_cmd(&sock_path, "j/activewindow");
        if !active_raw.is_empty() {
            if let Ok(active_obj) = serde_json::from_str::<Value>(&active_raw) {
                if let Some(a) = active_obj.get("address").and_then(|v| v.as_str()) {
                    active_addr = a.to_lowercase();
                }
            }
        }

        let Some(target_c) = pick_target(
            &matching,
            &visible_windows,
            &minimized_windows,
            &target_addr,
            target_index,
            &active_addr,
            Some(clients),
        ) else {
            return;
        };
        let target_c = target_c.clone();

        let addr = sfield(&target_c, "address");
        let is_min = ws_name(&target_c).starts_with("special:");

        // 用户自建抽屉（如 scratchpad）：展开抽屉即可，窗口不搬出
        let src_ws_name = ws_name(&target_c);
        if is_min && src_ws_name != "special:minimized" {
            reveal_user_drawer(&sock_path, &src_ws_name, &addr);
            return;
        }

        if is_min {
            // Rule §4.1: Repeat right-click on minimized app is strictly ignored
            if mode == "minimize" || mode == "minimize-instance" {
                return;
            }

            // RESTORE target_c to current focused workspace
            restore_window(&sock_path, &addr, &target_c, &focused_ws);
        } else {
            // MINIMIZE target_c
            let res = hypr_cmd(
                &sock_path,
                &format!(
                    r#"dispatch hl.dsp.window.move({{ window = "address:{addr}", workspace = "special:minimized" }})"#
                ),
            );
            if res.to_lowercase().contains("error") {
                hypr_cmd(
                    &sock_path,
                    &format!("dispatch movetoworkspacesilent special:minimized,address:{addr}"),
                );
            }

            close_any_special(&sock_path);

            let other_visible: Vec<&Value> = visible_windows
                .iter()
                .filter(|c| addr_of(c) != addr.to_lowercase())
                .collect();
            if !other_visible.is_empty() {
                let sibling_addr = sfield(other_visible[0], "address");
                focus_window(&sock_path, &sibling_addr);
                if let Some(raise_sibling) = other_visible
                    .iter()
                    .find(|c| addr_of(c) == sibling_addr.to_lowercase())
                {
                    raise_window(&sock_path, raise_sibling);
                }
            } else {
                let remaining: Vec<&Value> = clients
                    .iter()
                    .filter(|c| {
                        if addr_of(c) == addr.to_lowercase() {
                            return false;
                        }
                        let n = ws_name(c);
                        if n.starts_with("special:") {
                            return false;
                        }
                        let id = match c.get("workspace").and_then(|w| w.get("id")) {
                            Some(Value::Number(num)) => num.to_string(),
                            Some(Value::String(s)) => s.clone(),
                            _ => String::new(),
                        };
                        let eff = if !n.is_empty() { n } else { id };
                        eff == focused_ws
                    })
                    .collect();
                if let Some(first) = remaining.first() {
                    let t_addr = sfield(first, "address");
                    hypr_cmd(
                        &sock_path,
                        &format!(r#"dispatch hl.dsp.focus({{ window = "address:{t_addr}" }})"#),
                    );
                }
            }
        }
        return;
    }

    if [
        "activate-instance",
        "activate",
        "restore",
        "restore-or-launch",
    ]
    .contains(&mode)
    {
        if matching.is_empty() {
            launch_fallback(&queries);
            return;
        }

        let Some(target_c) = pick_target(
            &matching,
            &visible_windows,
            &minimized_windows,
            &target_addr,
            target_index,
            "",
            Some(clients),
        ) else {
            return;
        };
        let target_c = target_c.clone();

        let addr = sfield(&target_c, "address");
        let is_min = ws_name(&target_c).starts_with("special:");

        // 用户自建抽屉（如 scratchpad）：展开抽屉即可，窗口不搬出
        let src_ws_name = ws_name(&target_c);
        if is_min && src_ws_name != "special:minimized" {
            reveal_user_drawer(&sock_path, &src_ws_name, &addr);
            return;
        }

        if is_min {
            restore_window(&sock_path, &addr, &target_c, &focused_ws);
        } else {
            focus_window(&sock_path, &addr);
            raise_window(&sock_path, &target_c);
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    run(&args);
}

// ---------------------------------------------------------------------------
// Tests — mirrored from tests/test_dock_minimize.py
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    fn client(addr: &str, workspace: &str) -> Value {
        json!({"address": addr, "workspace": {"name": workspace}})
    }

    #[test]
    fn split_queries_address_is_selector() {
        let qs: Vec<String> = ["firefox", "firefox.desktop", "/usr/bin/firefox", "0xDEADBEEF"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (identifiers, index, addr) = split_queries(&qs);
        assert_eq!(identifiers, vec!["firefox", "firefox.desktop", "/usr/bin/firefox"]);
        assert_eq!(index, -1);
        assert_eq!(addr, "0xdeadbeef");
    }

    #[test]
    fn split_queries_legacy_index() {
        let qs: Vec<String> = ["firefox", "--index=2"].iter().map(|s| s.to_string()).collect();
        let (identifiers, index, addr) = split_queries(&qs);
        assert_eq!(identifiers, vec!["firefox"]);
        assert_eq!(index, 2);
        assert_eq!(addr, "");

        let qs: Vec<String> = ["firefox", "2"].iter().map(|s| s.to_string()).collect();
        let (identifiers, index, addr) = split_queries(&qs);
        assert_eq!(identifiers, vec!["firefox"]);
        assert_eq!(index, 2);
        assert_eq!(addr, "");
    }

    #[test]
    fn split_queries_no_selector() {
        let qs: Vec<String> = ["firefox"].iter().map(|s| s.to_string()).collect();
        let (identifiers, index, addr) = split_queries(&qs);
        assert_eq!(identifiers, vec!["firefox"]);
        assert_eq!(index, -1);
        assert_eq!(addr, "");
    }

    #[test]
    fn split_queries_hex_looking_identifier_kept() {
        let qs: Vec<String> = ["deface", "facade"].iter().map(|s| s.to_string()).collect();
        let (identifiers, _, addr) = split_queries(&qs);
        assert_eq!(identifiers, vec!["deface", "facade"]);
        assert_eq!(addr, "");
    }

    fn pick_fixture() -> (Vec<Value>, Vec<Value>, Vec<Value>) {
        let first = client("0xAAA", "1");
        let second = client("0xBBB", "1");
        let hidden = client("0xCCC", "special:minimized");
        let matching = vec![first.clone(), second.clone(), hidden.clone()];
        let visible = vec![first, second];
        let minimized = vec![hidden];
        (matching, visible, minimized)
    }

    #[test]
    fn pick_target_address_wins() {
        let (matching, visible, minimized) = pick_fixture();
        let picked = pick_target(&matching, &visible, &minimized, "0xbbb", -1, "", None).unwrap();
        assert_eq!(addr_of(picked), "0xbbb");
    }

    #[test]
    fn pick_target_address_beats_index() {
        let (matching, visible, minimized) = pick_fixture();
        let picked = pick_target(&matching, &visible, &minimized, "0xbbb", 0, "", None).unwrap();
        assert_eq!(addr_of(picked), "0xbbb");
    }

    #[test]
    fn pick_target_index_when_no_address() {
        let (matching, visible, minimized) = pick_fixture();
        let picked = pick_target(&matching, &visible, &minimized, "", 1, "", None).unwrap();
        assert_eq!(addr_of(picked), "0xbbb");
    }

    #[test]
    fn pick_target_gone_address_falls_through() {
        let (matching, visible, minimized) = pick_fixture();
        let picked =
            pick_target(&matching, &visible, &minimized, "0xffff", -1, "0xbbb", None).unwrap();
        assert_eq!(addr_of(picked), "0xbbb");
    }

    #[test]
    fn pick_target_active_before_visible() {
        let (matching, visible, minimized) = pick_fixture();
        let picked = pick_target(&matching, &visible, &minimized, "", -1, "0xbbb", None).unwrap();
        assert_eq!(addr_of(picked), "0xbbb");
    }

    #[test]
    fn pick_target_first_visible_default() {
        let (matching, visible, minimized) = pick_fixture();
        let picked = pick_target(&matching, &visible, &minimized, "", -1, "", None).unwrap();
        assert_eq!(addr_of(picked), "0xaaa");
    }

    #[test]
    fn pick_target_minimized_last_resort() {
        let (matching, _visible, minimized) = pick_fixture();
        let picked = pick_target(&matching, &[], &minimized, "", -1, "", None).unwrap();
        assert_eq!(addr_of(picked), "0xccc");
    }

    #[test]
    fn pick_target_nothing_matched() {
        assert!(pick_target(&[], &[], &[], "", -1, "", None).is_none());
    }

    #[test]
    fn pick_target_explicit_address_from_all_clients() {
        let target_win = client("0xTARGET", "1");
        let other_win = client("0xOTHER", "1");
        let all_clients = vec![other_win, target_win.clone()];
        let picked =
            pick_target(&[], &[], &[], "0xtarget", -1, "", Some(&all_clients)).unwrap();
        assert_eq!(picked, &target_win);
    }

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn temp_test_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dock-minimize-rs-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn find_desktop_file_with_spaces() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmpdir = temp_test_dir("spaces");
        let app_dir = tmpdir.join("applications");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(app_dir.join("My Custom App.desktop"), "[Desktop Entry]\nName=My Custom App\nType=Application\nExec=true\n").unwrap();
        let old_xdg = env::var("XDG_DATA_DIRS").ok();
        env::set_var("XDG_DATA_DIRS", &tmpdir);

        assert_eq!(find_desktop_file("My Custom App"), "My Custom App.desktop");
        assert_eq!(find_desktop_file("My Custom App.desktop"), "My Custom App.desktop");

        match old_xdg {
            Some(v) => env::set_var("XDG_DATA_DIRS", v),
            None => env::remove_var("XDG_DATA_DIRS"),
        }
        let _ = fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn find_desktop_file_entry_id_ending_in_desktop() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmpdir = temp_test_dir("telegram");
        let app_dir = tmpdir.join("applications");
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(app_dir.join("org.telegram.desktop.desktop"), "[Desktop Entry]\nName=Telegram\nType=Application\nExec=true\n").unwrap();
        let old_xdg = env::var("XDG_DATA_DIRS").ok();
        env::set_var("XDG_DATA_DIRS", &tmpdir);

        assert_eq!(find_desktop_file("org.telegram.desktop"), "org.telegram.desktop.desktop");

        match old_xdg {
            Some(v) => env::set_var("XDG_DATA_DIRS", v),
            None => env::remove_var("XDG_DATA_DIRS"),
        }
        let _ = fs::remove_dir_all(&tmpdir);
    }

    #[test]
    fn find_desktop_file_rejects_traversal_and_null() {
        assert_eq!(find_desktop_file("/etc/passwd"), "");
        assert_eq!(find_desktop_file("../app.desktop"), "");
        assert_eq!(find_desktop_file("app\0name"), "");
        assert_eq!(find_desktop_file(""), "");
    }

    #[test]
    fn reverse_dns_terminal_app_ids_are_terminals() {
        for app_id in [
            "com.mitchellh.ghostty",
            "org.kde.konsole",
            "com.gexperts.tilix",
            "org.gnome.ptyxis",
            "com.raggesilver.blackbox",
        ] {
            assert!(is_terminal_identifier(app_id), "{app_id} should be a terminal");
        }
    }

    #[test]
    fn plain_terminal_names_still_match() {
        for name in ["ghostty", "foot", "kitty", "alacritty"] {
            assert!(is_terminal_identifier(name), "{name} should be a terminal");
        }
    }

    #[test]
    fn non_terminals_are_not_terminals() {
        for app_id in ["firefox", "org.mozilla.firefox", "org.gnome.Nautilus"] {
            assert!(!is_terminal_identifier(app_id), "{app_id} should not be a terminal");
        }
    }

    #[test]
    fn real_world_chrome_pwa_classes() {
        let q_teams = r#"google-chrome-stable --profile-directory="Profile 1" --app="https://teams.microsoft.com/""#;
        assert!(match_chrome_pwa("chrome-teams.microsoft.com__-Profile_1", q_teams));

        let q_outlook = r#"google-chrome-stable --profile-directory="Profile 1" --app="https://outlook.office.com/mail/""#;
        assert!(match_chrome_pwa("chrome-outlook.office.com__mail_-Profile_1", q_outlook));

        let q_excel = r#"google-chrome-stable --profile-directory="Profile 1" --app="https://excel.cloud.microsoft/""#;
        assert!(match_chrome_pwa("chrome-excel.cloud.microsoft__-Profile_1", q_excel));
    }

    #[test]
    fn pwa_profile_isolation() {
        let q_prof2 = r#"google-chrome-stable --profile-directory="Profile 2" --app="https://outlook.office.com/mail/""#;
        assert!(!match_chrome_pwa("chrome-outlook.office.com__mail_-Profile_1", q_prof2));
        assert!(match_chrome_pwa("chrome-outlook.office.com__mail_-Profile_2", q_prof2));
    }

    #[test]
    fn pwa_fallback_when_no_profile() {
        let q_no_prof = r#"google-chrome-stable --app="https://outlook.office.com/mail/""#;
        assert!(match_chrome_pwa("chrome-outlook.office.com__mail_-Profile_1", q_no_prof));
        assert!(match_chrome_pwa("chrome-outlook.office.com__mail_-Default", q_no_prof));
    }

    #[test]
    fn pwa_browser_variants() {
        let q = r#"brave-browser --app="https://teams.microsoft.com/""#;
        assert!(match_chrome_pwa("brave-teams.microsoft.com__-Default", q));
        let q_edge = r#"microsoft-edge --app="https://teams.microsoft.com/""#;
        assert!(match_chrome_pwa("msedge-teams.microsoft.com__-Default", q_edge));
        assert!(match_chrome_pwa("edge-teams.microsoft.com__-Default", q_edge));
    }

    #[test]
    fn pwa_app_id() {
        let q = r#"google-chrome-stable --profile-directory="Default" --app-id="appgkjomdnhhdolojlpkjafpklojikld""#;
        assert!(match_chrome_pwa("chrome-appgkjomdnhhdolojlpkjafpklojikld-Default", q));
        assert!(!match_chrome_pwa("chrome-appgkjomdnhhdolojlpkjafpklojikld-Profile_1", q));
    }

    #[test]
    fn pwa_non_pwa_does_not_match() {
        assert!(!match_chrome_pwa("foot", "foot -e nvim"));
        assert!(!match_chrome_pwa("google-chrome", "google-chrome-stable"));
    }

    #[test]
    fn scan_disk_icons_finds_system_icons() {
        let icons = scan_disk_icons();
        assert!(!icons.is_empty());
        if Path::new("/usr/share/icons/hicolor/scalable/apps/omanta.svg").exists() {
            assert!(icons.get("omanta").unwrap().ends_with(".svg"));
        }
        if Path::new("/usr/share/icons/hicolor/scalable/apps/omashow.svg").exists() {
            assert!(icons.get("omashow").unwrap().ends_with(".svg"));
        }
    }

    #[test]
    fn normalize_matches_python_semantics() {
        assert_eq!(normalize("org.telegram.desktop"), "telegram");
        assert_eq!(normalize("com.mitchellh.ghostty"), "mitchellhghostty");
        assert_eq!(normalize("Firefox.desktop"), "firefox");
        assert_eq!(normalize("Some App.exe"), "someapp");
    }
}
