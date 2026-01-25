use rand::{distributions::Alphanumeric, Rng};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub fn path_exists(path: impl AsRef<Path>) -> bool {
    fs::metadata(path).is_ok()
}

pub fn ensure_dir_for_file(path: impl AsRef<Path>) -> io::Result<()> {
    if let Some(parent) = path.as_ref().parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub fn temp_sibling_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("temp");
    let token: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();
    parent.join(format!("{}.{}.tmp", file_name, token))
}

pub fn atomic_write_text_file(path: impl AsRef<Path>, content: &str, mode: u32) -> io::Result<()> {
    let path = path.as_ref();
    ensure_dir_for_file(path)?;
    let tmp = temp_sibling_path(path);
    {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        }
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(tmp, path)?;
    Ok(())
}

pub fn atomic_write_binary_file(
    path: impl AsRef<Path>,
    content: &[u8],
    mode: u32,
) -> io::Result<()> {
    let path = path.as_ref();
    ensure_dir_for_file(path)?;
    let tmp = temp_sibling_path(path);
    {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        }
        file.write_all(content)?;
        file.sync_all()?;
    }
    fs::rename(tmp, path)?;
    Ok(())
}

pub fn atomic_replace_file(path: impl AsRef<Path>, content: &str, mode: u32) -> io::Result<()> {
    atomic_write_text_file(path, content, mode)
}
