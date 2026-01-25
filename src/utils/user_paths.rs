use std::path::{Path, PathBuf};

pub fn expand_home_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if let Some(str_path) = path.to_str() {
        if let Some(rest) = str_path.strip_prefix("~/") {
            if let Ok(home) = std::env::var("HOME") {
                return PathBuf::from(home).join(rest);
            }
        }
        if str_path == "~" {
            if let Ok(home) = std::env::var("HOME") {
                return PathBuf::from(home);
            }
        }
    }
    path.to_path_buf()
}
