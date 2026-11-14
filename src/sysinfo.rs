//! System introspection: live CPU/RAM polling, process listing/dumping,
//! disk + folder size scanning, and coding-toolchain detection.
//!
//! Used by (a) the status bar meter, (b) the agent's `process_*`/`storage_scan`
//! tools so Claude can stay efficient, and (c) the `/env` wizard.

use std::path::Path;
use std::time::{Duration, Instant};

// ------------------------------------------------------------ live poller

/// Lightweight background poller for the status bar (CPU + RAM).
/// Refreshes at most every 2 s so it stays cheap.
pub struct SystemPoller {
    sys: sysinfo::System,
    last: Instant,
    cpu: f32,
    ram_used: u64,
    ram_total: u64,
}

impl SystemPoller {
    pub fn new() -> Self {
        let mut sys = sysinfo::System::new().expect("sysinfo init");
        sys.refresh_memory();
        sys.refresh_cpu_all();
        Self { sys, last: Instant::now() - Duration::from_secs(10), cpu: 0.0, ram_used: 0, ram_total: 0 }
    }

    /// Call every frame; actually refreshes on a 2 s cadence.
    pub fn tick(&mut self) {
        if self.last.elapsed() >= Duration::from_secs(2) {
            self.last = Instant::now();
            self.sys.refresh_memory();
            self.sys.refresh_cpu_all();
            self.cpu = self.sys.global_cpu_usage();
            self.ram_used = self.sys.used_memory();
            self.ram_total = self.sys.total_memory();
        }
    }

    /// (cpu_percent, ram_used, ram_total)
    pub fn snapshot(&self) -> (f32, u64, u64) {
        (self.cpu, self.ram_used, self.ram_total)
    }
}

// ---------------------------------------------------------------- processes

/// Table of top processes as plain text (for the `process_list` tool).
pub fn snapshot_processes(limit: usize, by_memory: bool) -> String {
    let mut sys = sysinfo::System::new().expect("sysinfo init");
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );

    let mut procs: Vec<_> = sys
        .processes()
        .values()
        .map(|p| (p.name().to_string_lossy().to_string(), p.cpu_usage(), p.memory()))
        .collect();
    if by_memory {
        procs.sort_by_key(|p| std::cmp::Reverse(p.2));
    } else {
        procs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    }

    let mut out = String::from("PID        NAME                          CPU%     MEM\n");
    for (i, (name, cpu, mem)) in procs.iter().enumerate().take(limit) {
        out.push_str(&format!(
            "{:<10} {:<28} {:>6.1}%  {}\n",
            i + 1,
            truncate(name, 28),
            cpu,
            crate::util::human_size(*mem)
        ));
    }
    let (used, total) = memory_totals();
    out.push_str(&format!("\nSystem RAM: {} / {} used\n", crate::util::human_size(used), crate::util::human_size(total)));
    out
}

/// Full JSON dump (system + process table) for `process_dump`.
pub fn dump_json(by_memory: bool) -> String {
    let mut sys = sysinfo::System::new().expect("sysinfo init");
    sys.refresh_all();

    let mut procs: Vec<(&sysinfo::Pid, &sysinfo::Process)> = sys.processes().iter().collect();
    if by_memory {
        procs.sort_by_key(|p| std::cmp::Reverse(p.1.memory()));
    } else {
        procs.sort_by(|a, b| {
            b.1.cpu_usage().partial_cmp(&a.1.cpu_usage()).unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    let v = serde_json::json!({
        "generated_at": chrono::Local::now().to_rfc3339(),
        "host": {
            "os": sysinfo::System::name().unwrap_or_default(),
            "os_version": sysinfo::System::os_version().unwrap_or_default(),
            "kernel": sysinfo::System::kernel_version().unwrap_or_default(),
            "hostname": sysinfo::System::host_name().unwrap_or_default(),
            "cpu_cores": sysinfo::System::physical_core_count().unwrap_or(0),
            "ram_total_bytes": sys.total_memory(),
            "ram_used_bytes": sys.used_memory(),
        },
        "processes": procs.iter().take(200).map(|(pid, p)| serde_json::json!({
            "pid": pid.as_u32(),
            "name": p.name().to_string_lossy(),
            "cpu_percent": p.cpu_usage(),
            "memory_bytes": p.memory(),
            "status": format!("{:?}", p.status()),
            "exe": p.exe().map(|e| e.display().to_string()).unwrap_or_default(),
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into())
}

/// (used, total) RAM in bytes.
pub fn memory_totals() -> (u64, u64) {
    let mut sys = sysinfo::System::new().expect("sysinfo init");
    sys.refresh_memory();
    (sys.used_memory(), sys.total_memory())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max - 1).collect::<String>() + "…"
    }
}

// ----------------------------------------------------------------- storage

/// Disks overview + biggest first-level subdirectories under `path`.
pub fn storage_scan(path: &Path) -> String {
    let mut out = String::new();

    // Disks overview
    out.push_str("Disks:\n");
    let disks = sysinfo::Disks::new_with_refreshed_list().expect("disk init");
    for d in disks.iter() {
        out.push_str(&format!(
            "  {:<8} total {:>9}  free {:>9}\n",
            d.mount_point().display(),
            crate::util::human_size(d.total_space()),
            crate::util::human_size(d.available_space()),
        ));
    }

    // Top-level subdirectory sizes under `path`
    out.push_str(&format!("\nBiggest folders under {}:\n", path.display()));
    let mut rows: Vec<(String, u64)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = e.file_name().to_string_lossy().to_string();
                rows.push((name, dir_size(&p, 20_000)));
            }
        }
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    let total: u64 = rows.iter().map(|r| r.1).sum();
    for (name, size) in rows.iter().take(25) {
        if *size == 0 {
            continue;
        }
        out.push_str(&format!("  {:>10}  {}\n", crate::util::human_size(*size), name));
    }
    out.push_str(&format!("  {:>10}  (total, top level)\n", crate::util::human_size(total)));
    out
}

/// Recursive directory size with a file-count cap so it can't run away.
/// (On very large trees the result is a lower bound — documented behavior.)
fn dir_size(dir: &Path, cap: u64) -> u64 {
    let mut seen = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            if seen >= cap {
                return seen; // cap reached: report partial (lower bound)
            }
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(md) = e.metadata() {
                seen += md.len();
            }
        }
    }
    seen
}

// --------------------------------------------------------------- toolchains

/// Structured detection: (name, installed, version-or-empty).
pub fn detect_structured() -> Vec<(String, bool, String)> {
    let probes: &[(&str, &str, &[&str])] = &[
        ("node", "node", &["--version"]),
        ("npm", "npm", &["--version"]),
        ("pnpm", "pnpm", &["--version"]),
        ("bun", "bun", &["--version"]),
        ("python", "python", &["--version"]),
        ("pip", "pip", &["--version"]),
        ("rust", "rustc", &["--version"]),
        ("cargo", "cargo", &["--version"]),
        ("go", "go", &["version"]),
        ("dotnet", "dotnet", &["--version"]),
        ("java", "java", &["-version"]),
        ("git", "git", &["--version"]),
    ];
    probes
        .iter()
        .map(|(name, prog, args)| match run_version(prog, args) {
            Some(v) => (name.to_string(), true, v),
            None => (name.to_string(), false, String::new()),
        })
        .collect()
}

/// Detect coding toolchains by invoking `--version`. Returns a report.
pub fn detect_toolchains() -> String {
    let rows = detect_structured();
    let found: Vec<String> = rows
        .iter()
        .filter(|(_, ok, _)| *ok)
        .map(|(n, _, v)| format!("  ✓ {n:<8} {v}"))
        .collect();
    let missing: Vec<String> = rows
        .iter()
        .filter(|(_, ok, _)| !*ok)
        .map(|(n, _, _)| n.clone())
        .collect();

    let mut out = String::from("Installed toolchains:\n");
    out.push_str(&found.join("\n"));
    if !missing.is_empty() {
        out.push_str(&format!(
            "\n\nMissing: {} — offer `env_install` for any of these.",
            missing.join(", ")
        ));
    }
    out
}

/// Run `prog args` capturing stdout+stderr; None if not installed.
fn run_version(prog: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(prog).args(args).output().ok()?;
    if !out.status.success() && out.stdout.is_empty() && out.stderr.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let text = if text.trim().is_empty() { String::from_utf8_lossy(&out.stderr).to_string() } else { text.to_string() };
    Some(crate::util::one_line(&text, 60))
}
