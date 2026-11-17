//! Small shared helpers: Windows folder-picker dialog, desktop notifications,
//! and misc string/path utilities used across modules.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Expand `~` and leading env vars so users can type `~/projects` etc.
pub fn expand_path(p: &str) -> PathBuf {
    let mut s = p.trim().to_string();
    if s == "~"
        && let Some(home) = dirs::home_dir() {
            return home;
        }
    if let Some(home) = dirs::home_dir()
        && let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
            s = home.join(rest).to_string_lossy().to_string();
        }
    PathBuf::from(s)
}

/// Human readable byte size.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    for u in UNITS {
        if v < 1024.0 {
            return format!("{v:.1} {u}");
        }
        v /= 1024.0;
    }
    format!("{v:.1} PB")
}

/// Human readable token counts (12.3k / 1.2M).
pub fn human_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Open a native Windows folder-browse dialog (Explorer style).
/// Returns `None` if the user cancels. Works via PowerShell STA + WinForms,
/// so no extra Rust dependencies are needed.
pub fn browse_folder(title: &str) -> Result<Option<PathBuf>> {
    let title = title.replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
         $d.Description = '{title}'; \
         $d.ShowNewFolderButton = $false; \
         if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{ Write-Output $d.SelectedPath }}",
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .output()
        .context("failed to launch folder dialog (powershell)")?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(s)))
    }
}

/// Open a URL in the user's default browser.
pub fn open_browser(url: &str) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // the empty "" is `start`'s window title, so the URL isn't taken as one
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .context("failed to open browser")?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn().context("failed to open browser")?;
    }
    Ok(())
}

/// Escape a string for embedding inside single-quoted PowerShell literals.
fn ps_escape(s: &str) -> String {
    s.replace('\'', "''").replace(['\r', '\n'], " ")
}

/// Escape text placed inside the toast's XML (`<`, `&` would break LoadXml).
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Send a Windows toast notification (falls back to a message box if WinRT
/// toasts are unavailable). Used by `/later` reminders and the `notify` tool.
///
/// The notifier AUMID must be registered with Windows or the toast is
/// dropped silently — reusing PowerShell's own registered app id works on
/// every stock Windows install.
pub fn send_toast(title: &str, message: &str) -> Result<bool> {
    // the toast gets XML-escaped text; the message-box fallback gets it raw
    let (title_raw, message_raw) = (ps_escape(title), ps_escape(message));
    let (title, message) = (ps_escape(&xml_escape(title)), ps_escape(&xml_escape(message)));
    let script = format!(
        "try {{ \
           $ErrorActionPreference = 'Stop'; \
           [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null; \
           $xml = New-Object Windows.Data.Xml.Dom.XmlDocument; \
           $xml.LoadXml('<toast><visual><binding template=\"ToastGeneric\"><text>{title}</text><text>{message}</text></binding></visual></toast>'); \
           $t = [Windows.UI.Notifications.ToastNotification]::new($xml); \
           [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}}\\WindowsPowerShell\\v1.0\\powershell.exe').Show($t); \
           Write-Output 'toast-ok' \
         }} catch {{ \
           Add-Type -AssemblyName System.Windows.Forms; \
           [System.Windows.Forms.MessageBox]::Show('{message_raw}', '{title_raw}') | Out-Null; \
           Write-Output 'msgbox-ok' \
         }}"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .output()
        .context("failed to launch notification")?;
    let s = String::from_utf8_lossy(&out.stdout);
    Ok(s.contains("toast-ok") || s.contains("msgbox-ok"))
}

/// Truncate a string on char boundaries (no UTF-8 panics).
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// Collapse a string to a single line, truncating if needed (for tool previews).
pub fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&flat, max)
}
