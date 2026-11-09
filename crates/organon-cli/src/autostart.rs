//! Per-user OS service for the continuously running watcher.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use organon_core::config::OrgConfig;

use crate::cli_types::AutostartCmd;

#[cfg(target_os = "macos")]
const LABEL: &str = "dev.organon.watch";

pub(crate) fn cmd_autostart(action: AutostartCmd, config: &OrgConfig, db: &Path) -> Result<()> {
    let home = user_home()?;
    let service = service_path(&home)?;
    match action {
        AutostartCmd::Install => {
            if config.watch.roots.is_empty() {
                bail!(
                    "configure [watch].roots in ~/.organon/config.toml before enabling autostart"
                );
            }
            for root in &config.watch.roots {
                if !root.is_absolute() || !root.is_dir() {
                    bail!(
                        "watch root must be an existing absolute directory: {}",
                        root.display()
                    );
                }
            }
            let exe = fs::canonicalize(std::env::current_exe()?)?;
            let db = absolute_path(db)?;
            let config_path = std::env::var_os("ORGANON_CONFIG")
                .map(PathBuf::from)
                .map(|p| absolute_path(&p))
                .transpose()?;
            let organon_home = std::env::var_os("ORGANON_HOME")
                .map(PathBuf::from)
                .map(|p| absolute_path(&p))
                .transpose()?;
            let extra_sensitive = std::env::var("ORGANON_SENSITIVE_EXTRA").ok();
            let project = std::env::var_os("ORGANON_PYTHON_PROJECT")
                .map(PathBuf::from)
                .map(|p| absolute_path(&p))
                .transpose()?;
            let uv = std::env::var_os("ORGANON_UV")
                .map(PathBuf::from)
                .map(|p| {
                    if p.components().count() == 1 {
                        executable_on_path(&p.to_string_lossy()).unwrap_or(p)
                    } else {
                        p
                    }
                })
                .or_else(|| executable_on_path("uv"));
            let uv = uv.map(|p| absolute_path(&p)).transpose()?;
            let env = [
                ("HOME", Some(home.to_string_lossy().into_owned())),
                (
                    "ORGANON_CONFIG",
                    config_path.map(|p| p.to_string_lossy().into_owned()),
                ),
                (
                    "ORGANON_HOME",
                    organon_home.map(|p| p.to_string_lossy().into_owned()),
                ),
                ("ORGANON_SENSITIVE_EXTRA", extra_sensitive),
                (
                    "ORGANON_PYTHON_PROJECT",
                    project.map(|p| p.to_string_lossy().into_owned()),
                ),
                ("ORGANON_UV", uv.map(|p| p.to_string_lossy().into_owned())),
                ("PATH", std::env::var("PATH").ok()),
            ];
            let body = render_service(&exe, &db, &home, &env)?;
            if let Some(parent) = service.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::create_dir_all(organon_home_dir(&home).join("daemon"))?;
            if service.exists() {
                stop_service(&service)?;
            }
            fs::write(&service, body).with_context(|| format!("writing {}", service.display()))?;
            start_service(&service)?;
            println!("autostart enabled: {}", service.display());
        }
        AutostartCmd::Uninstall => {
            if !service.exists() {
                println!("autostart is not installed");
                return Ok(());
            }
            stop_service(&service)?;
            fs::remove_file(&service)?;
            #[cfg(target_os = "linux")]
            run("systemctl", &["--user", "daemon-reload"])?;
            println!("autostart removed: {}", service.display());
        }
    }
    Ok(())
}

fn user_home() -> Result<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    if !home.is_absolute() {
        bail!("HOME must be an absolute path");
    }
    Ok(home)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn executable_on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

#[cfg(target_os = "macos")]
fn service_path(home: &Path) -> Result<PathBuf> {
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

#[cfg(target_os = "linux")]
fn service_path(home: &Path) -> Result<PathBuf> {
    Ok(home.join(".config/systemd/user/organon.service"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn service_path(_home: &Path) -> Result<PathBuf> {
    bail!("autostart supports macOS and Linux only")
}

fn organon_home_dir(home: &Path) -> PathBuf {
    std::env::var_os("ORGANON_HOME")
        .map(|value| {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| home.to_path_buf())
                    .join(path)
            }
        })
        .unwrap_or_else(|| home.join(".organon"))
}

#[cfg(target_os = "macos")]
fn render_service(
    exe: &Path,
    db: &Path,
    home: &Path,
    env: &[(&str, Option<String>)],
) -> Result<String> {
    let log = organon_home_dir(home).join("daemon/watch-autostart.log");
    let mut vars = String::new();
    for (key, value) in env {
        if let Some(value) = value {
            vars.push_str(&format!("<key>{key}</key><string>{}</string>", xml(value)));
        }
    }
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{LABEL}</string>\n<key>ProgramArguments</key><array><string>{}</string><string>--db</string><string>{}</string><string>watch</string><string>--detect-renames</string></array>\n<key>EnvironmentVariables</key><dict>{vars}</dict>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><true/>\n<key>StandardOutPath</key><string>{}</string>\n<key>StandardErrorPath</key><string>{}</string>\n</dict></plist>\n",
        xml(&exe.to_string_lossy()), xml(&db.to_string_lossy()),
        xml(&log.to_string_lossy()), xml(&log.to_string_lossy())
    ))
}

#[cfg(target_os = "macos")]
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "linux")]
fn render_service(
    exe: &Path,
    db: &Path,
    home: &Path,
    env: &[(&str, Option<String>)],
) -> Result<String> {
    let log = organon_home_dir(home).join("daemon/watch-autostart.log");
    let mut vars = String::new();
    for (key, value) in env {
        if let Some(value) = value {
            vars.push_str(&format!(
                "Environment=\"{key}={}\"\n",
                systemd_escape(value)?
            ));
        }
    }
    Ok(format!(
        "[Unit]\nDescription=Organon filesystem watcher\n\n[Service]\nType=simple\nExecStart=\"{}\" --db \"{}\" watch --detect-renames\n{vars}StandardOutput=append:{}\nStandardError=append:{}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
        systemd_escape(&exe.to_string_lossy())?, systemd_escape(&db.to_string_lossy())?,
        systemd_escape(&log.to_string_lossy())?, systemd_escape(&log.to_string_lossy())?
    ))
}

#[cfg(target_os = "linux")]
fn systemd_escape(value: &str) -> Result<String> {
    if value.contains(['\n', '\r', '\0']) {
        bail!("service paths and environment values cannot contain newlines or NUL");
    }
    Ok(value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%"))
}

#[cfg(target_os = "macos")]
fn start_service(path: &Path) -> Result<()> {
    let target = format!("gui/{}", unsafe { libc::getuid() });
    run(
        "launchctl",
        &["bootstrap", &target, &path.to_string_lossy()],
    )
}

#[cfg(target_os = "macos")]
fn stop_service(_path: &Path) -> Result<()> {
    let target = format!("gui/{}/{}", unsafe { libc::getuid() }, LABEL);
    if !Command::new("launchctl")
        .args(["print", &target])
        .output()?
        .status
        .success()
    {
        return Ok(());
    }
    run("launchctl", &["bootout", &target])
}

#[cfg(target_os = "linux")]
fn start_service(_path: &Path) -> Result<()> {
    run("systemctl", &["--user", "daemon-reload"])?;
    run(
        "systemctl",
        &["--user", "enable", "--now", "organon.service"],
    )
}

#[cfg(target_os = "linux")]
fn stop_service(_path: &Path) -> Result<()> {
    run(
        "systemctl",
        &["--user", "disable", "--now", "organon.service"],
    )
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_renders_paths_and_config() {
        let env = [("ORGANON_CONFIG", Some("/tmp/a b/config.toml".to_owned()))];
        let body = render_service(
            Path::new("/tmp/a b/organon"),
            Path::new("/tmp/db file"),
            Path::new("/tmp"),
            &env,
        )
        .unwrap();
        assert!(body.contains("/tmp/a b/organon"));
        assert!(body.contains("/tmp/db file"));
        assert!(body.contains("ORGANON_CONFIG"));
        assert!(body.contains("watch"));
        assert!(body.contains("watch-autostart.log"));
        assert!(!body.contains("--daemon"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_escapes_specifiers_and_quotes() {
        assert_eq!(systemd_escape("a%\"b").unwrap(), "a%%\\\"b");
        assert!(systemd_escape("a\nb").is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plist_escapes_xml() {
        assert_eq!(xml("a&<b"), "a&amp;&lt;b");
    }
}
