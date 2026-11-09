//! LaunchAgent management: run the menu bar app automatically (at login and
//! after crashes) via launchd, for any macOS user, without root.
//!
//! Layout (all in the current user's home):
//! - binary:  ~/.local/bin/budlink-mac   (copied from the running exe)
//! - plist:   ~/Library/LaunchAgents/<label>.plist
//! - logs:    ~/Library/Logs/budlink-mac.log

use std::path::PathBuf;

use anyhow::{bail, Context as _, Result};

const LABEL: &str = "budlink-mac";
const BIN_NAME: &str = "budlink-mac";

fn home() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .context("HOME not set")
}

fn bin_dir() -> Result<PathBuf> {
    Ok(home()?.join(".local/bin"))
}

fn plist_path() -> Result<PathBuf> {
    Ok(home()?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn log_path() -> Result<PathBuf> {
    Ok(home()?.join("Library/Logs/budlink-mac.log"))
}

fn plist(binary: &std::path::Path, log: &std::path::Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>menubar</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{}</string>
    <key>StandardErrorPath</key>
    <string>{}</string>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        binary.display(),
        log.display(),
        log.display()
    )
}

/// Copies the running binary to a stable location and (re)loads the agent.
pub fn install() -> Result<()> {
    let src = std::env::current_exe().context("cannot locate current executable")?;
    let dir = bin_dir()?;
    std::fs::create_dir_all(&dir).context("cannot create ~/.local/bin")?;
    let dst = dir.join(BIN_NAME);

    if dst.exists() {
        std::fs::remove_file(&dst).context("cannot replace installed binary")?;
    }
    std::fs::copy(&src, &dst).context("failed to copy binary to ~/.local/bin")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755))?;
    }

    // Remove quarantine attr if the source was ever downloaded/expanded.
    let _ = std::process::Command::new("xattr")
        .args(["-d", "com.apple.quarantine"])
        .arg(&dst)
        .output();

    let plist_file = plist_path()?;
    std::fs::create_dir_all(plist_file.parent().unwrap())?;
    let log = log_path()?;
    std::fs::create_dir_all(log.parent().unwrap())?;
    std::fs::write(&plist_file, plist(&dst, &log)).context("failed to write plist")?;

    // (Re)load: bootout first is fine if it was never loaded.
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &domain, &plist_file.to_string_lossy()])
        .output();
    let out = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain, &plist_file.to_string_lossy()])
        .output()
        .context("failed to run launchctl")?;
    if !out.status.success() {
        bail!("launchctl bootstrap failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    println!("Installed and started.");
    println!("  binary: {}", dst.display());
    println!("  plist:  {}", plist_file.display());
    println!("  logs:   {}", log.display());
    println!("The app now starts at login and restarts on crash.");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let plist_file = plist_path()?;
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &domain, &plist_file.to_string_lossy()])
        .output();
    if plist_file.exists() {
        std::fs::remove_file(&plist_file)?;
    }
    let dst = bin_dir()?.join(BIN_NAME);
    if dst.exists() {
        std::fs::remove_file(&dst)?;
    }
    println!("Uninstalled (logs kept at {}).", log_path()?.display());
    Ok(())
}

pub fn print_status() -> Result<()> {
    let plist_file = plist_path()?;
    println!("plist:    {} ({})", plist_file.display(),
        if plist_file.exists() { "installed" } else { "not installed" });
    let dst = bin_dir()?.join(BIN_NAME);
    println!("binary:   {} ({})", dst.display(),
        if dst.exists() { "installed" } else { "not installed" });
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let out = std::process::Command::new("launchctl")
        .args(["print", &format!("{domain}/{LABEL}")])
        .output()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout);
        for line in s.lines() {
            let t = line.trim();
            if t.starts_with("state =") || t.starts_with("pid =") || t.starts_with("last exit code =") {
                println!("{t}");
            }
        }
    } else {
        println!("agent:    not loaded");
    }
    Ok(())
}
