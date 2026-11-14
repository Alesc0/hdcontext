use anyhow::{Context, Result, ensure};
use std::{path::Path, process::Command};
const NAME: &str = "hdcontext-watch.service";
fn quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}
pub fn unit(executable: &Path, config: &Path) -> Result<String> {
    let executable = executable.to_str().context("App path is not valid UTF-8")?;
    let config = config
        .to_str()
        .context("Settings path is not valid UTF-8")?;
    Ok(format!(
        "[Unit]\nDescription=HDContext automatic gaming highlight scans\n\n[Service]\nType=simple\nExecStart={} watch --config {} --service\nRestart=on-failure\nRestartSec=10\nNice=10\nUMask=0077\n\n[Install]\nWantedBy=default.target\n",
        quote(executable),
        quote(config)
    ))
}
pub fn configure(enabled: bool, executable: &Path, config: &Path) -> Result<()> {
    ensure!(
        cfg!(target_os = "linux"),
        "Automatic background startup is currently supported on Linux. Run hdcontext watch manually on other platforms."
    );
    let base = directories::BaseDirs::new().context("Configuration directory unavailable")?;
    let directory = base.config_dir().join("systemd/user");
    if enabled {
        crate::scan_cache::private_dir(&directory)?;
        let mut file = tempfile::NamedTempFile::new_in(&directory)?;
        use std::io::Write;
        file.write_all(unit(executable, config)?.as_bytes())?;
        file.persist(directory.join(NAME))
            .map_err(|error| error.error)?;
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", NAME])
    } else {
        systemctl(&["disable", "--now", NAME])
    }
}
fn systemctl(arguments: &[&str]) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(arguments)
        .output()
        .context("Could not start systemctl --user")?;
    ensure!(
        output.status.success(),
        "Could not configure background scans: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unit_runs_only_the_headless_watcher_and_quotes_literal_paths() {
        let unit = unit(
            Path::new("/home/test/a $name%/hdcontext"),
            Path::new("/home/test/my \"settings\".json"),
        )
        .unwrap();
        assert!(unit.contains("watch --config"));
        assert!(unit.contains("--service"));
        assert!(unit.contains("$$name%%"));
        assert!(unit.contains("\\\"settings\\\""));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(!unit.contains("API_KEY"));
    }
}
