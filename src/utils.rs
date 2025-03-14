use std::{env, fs};
use std::path::PathBuf;

pub fn get_or_create_log_dir() -> PathBuf {
    let dir = {
        #[cfg(target_os = "macos")]
        {
            let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join("Library").join("Logs").join("slam")
        }
        #[cfg(not(target_os = "macos"))]
        {
            if let Ok(xdg_state) = env::var("XDG_STATE_HOME") {
                PathBuf::from(xdg_state).join("slam")
            } else if let Ok(home) = env::var("HOME") {
                PathBuf::from(home).join(".local").join("state").join("slam")
            } else {
                PathBuf::from("slam_logs")
            }
        }
    };

    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("Failed to create log directory {}: {}", dir.display(), e);
    }
    dir
}

pub fn indent(s: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    s.lines()
      .map(|line| format!("{}{}", pad, line))
      .collect::<Vec<_>>()
      .join("\n")
}
