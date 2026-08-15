use std::fs;
use std::ops::Range;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const HOOK_BEGIN: &str = "# >>> rgbpc sync hook >>>";
const HOOK_END: &str = "# <<< rgbpc sync hook <<<";
const HOOK_CONTENT: &str = "#!/bin/bash\nrgbpc --sync-theme &\n";
const RESTORE_AUTOSTART_DIR: &str = ".config/autostart";
const RESTORE_AUTOSTART_FILE: &str = "rgbpc-restore.desktop";
const RESTORE_AUTOSTART_CONTENT: &str = "[Desktop Entry]\nType=Application\nName=RGBPC Restore\nComment=Restore RGB lighting at login\nExec=rgbpc --restore-last\nTerminal=false\nX-GNOME-Autostart-enabled=true\n";

fn hook_path_from_home(home_dir: &Path) -> PathBuf {
    home_dir.join(".config/omarchy/hooks/theme-set.d/rgbpc")
}

fn legacy_hook_path_from_home(home_dir: &Path) -> PathBuf {
    home_dir.join(".config/omarchy/hooks/theme-set")
}

pub fn get_restore_autostart_path() -> Result<PathBuf, String> {
    Ok(restore_autostart_path_from_home(&user_home_dir()?))
}

fn restore_autostart_path_from_home(home_dir: &Path) -> PathBuf {
    home_dir
        .join(RESTORE_AUTOSTART_DIR)
        .join(RESTORE_AUTOSTART_FILE)
}

fn user_home_dir() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "Could not determine the user home directory".to_string())
}

pub fn install_hook() -> Result<(), String> {
    install_hook_at(&user_home_dir()?)
}

fn install_hook_at(home_dir: &Path) -> Result<(), String> {
    let path = hook_path_from_home(home_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    match fs::read_to_string(&path) {
        Ok(existing) if existing == HOOK_CONTENT => {}
        Ok(_) => fs::write(&path, HOOK_CONTENT).map_err(|e| e.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::write(&path, HOOK_CONTENT).map_err(|e| e.to_string())?
        }
        Err(error) => return Err(error.to_string()),
    }

    let mut perms = fs::metadata(&path)
        .map_err(|e| e.to_string())?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).map_err(|e| e.to_string())?;

    remove_legacy_hook_at(home_dir)?;

    Ok(())
}

pub fn migrate_hook_if_needed() -> Result<(), String> {
    migrate_hook_if_needed_at(&user_home_dir()?)
}

fn migrate_hook_if_needed_at(home_dir: &Path) -> Result<(), String> {
    let hook_is_current = is_current_hook(&hook_path_from_home(home_dir))?;
    let legacy_hook_is_managed = legacy_hook_has_managed_block(home_dir)?;

    if hook_is_current && !legacy_hook_is_managed {
        return Ok(());
    }

    install_hook_at(home_dir)
}

fn is_current_hook(path: &Path) -> Result<bool, String> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };

    let mode = fs::metadata(path)
        .map_err(|e| e.to_string())?
        .permissions()
        .mode()
        & 0o777;

    Ok(content == HOOK_CONTENT && mode == 0o755)
}

pub fn remove_hook() -> Result<(), String> {
    remove_hook_at(&user_home_dir()?)
}

fn remove_hook_at(home_dir: &Path) -> Result<(), String> {
    remove_file_if_exists(&hook_path_from_home(home_dir))?;
    remove_legacy_hook_at(home_dir)
}

fn legacy_hook_has_managed_block(home_dir: &Path) -> Result<bool, String> {
    let path = legacy_hook_path_from_home(home_dir);
    match fs::read_to_string(path) {
        Ok(existing) => Ok(managed_block_range(&existing).is_some()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

fn remove_legacy_hook_at(home_dir: &Path) -> Result<(), String> {
    let path = legacy_hook_path_from_home(home_dir);
    let existing = match fs::read_to_string(&path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };

    let Some(updated) = remove_managed_block(&existing) else {
        return Ok(());
    };

    if updated.trim().is_empty() || updated.trim() == "#!/bin/bash" {
        remove_file_if_exists(&path)?;
        return Ok(());
    }

    fs::write(&path, updated).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn install_restore_autostart() -> Result<(), String> {
    let path = get_restore_autostart_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    fs::write(path, RESTORE_AUTOSTART_CONTENT).map_err(|e| e.to_string())
}

pub fn remove_restore_autostart() -> Result<(), String> {
    remove_file_if_exists(&get_restore_autostart_path()?)
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn managed_block_range(content: &str) -> Option<Range<usize>> {
    let start = content.find(HOOK_BEGIN)?;
    let mut end = start + content[start..].find(HOOK_END)? + HOOK_END.len();

    if content[end..].starts_with("\r\n") {
        end += 2;
    } else if content[end..].starts_with('\n') {
        end += 1;
    }

    Some(start..end)
}

fn remove_managed_block(content: &str) -> Option<String> {
    let range = managed_block_range(content)?;
    let mut updated = content.to_string();
    updated.replace_range(range, "");
    Some(updated)
}

#[cfg(test)]
mod tests {
    use super::{
        get_restore_autostart_path, hook_path_from_home, install_hook_at,
        legacy_hook_path_from_home, migrate_hook_if_needed_at, remove_hook_at,
        remove_managed_block, HOOK_CONTENT,
    };
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_HOME: AtomicU64 = AtomicU64::new(0);

    struct TestHome {
        path: PathBuf,
    }

    impl TestHome {
        fn new() -> Self {
            let unique = NEXT_TEST_HOME.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("rgbpc-hook-test-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn uses_quattro_hook_directory() {
        assert!(hook_path_from_home(Path::new("/home/test"))
            .ends_with(".config/omarchy/hooks/theme-set.d/rgbpc"));
    }

    #[test]
    fn removes_only_rgbpc_managed_block() {
        let content = "#!/bin/bash\necho pre\n# >>> rgbpc sync hook >>>\nrgbpc --sync-theme &\n# <<< rgbpc sync hook <<<\necho post\n";
        let updated = remove_managed_block(content).unwrap();
        assert_eq!(updated, "#!/bin/bash\necho pre\necho post\n");
    }

    #[test]
    fn returns_none_when_legacy_hook_is_not_owned_by_rgbpc() {
        assert_eq!(remove_managed_block("#!/bin/bash\necho custom\n"), None);
    }

    #[test]
    fn installs_an_executable_modular_hook() {
        let home = TestHome::new();
        install_hook_at(home.path()).unwrap();

        let hook_path = hook_path_from_home(home.path());
        assert_eq!(fs::read_to_string(&hook_path).unwrap(), HOOK_CONTENT);
        assert_eq!(
            fs::metadata(hook_path).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn migration_preserves_unrelated_legacy_commands() {
        let home = TestHome::new();
        let legacy_path = legacy_hook_path_from_home(home.path());
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(
            &legacy_path,
            "#!/bin/bash\necho pre\n# >>> rgbpc sync hook >>>\nrgbpc --sync-theme &\n# <<< rgbpc sync hook <<<\necho post\n",
        )
        .unwrap();

        migrate_hook_if_needed_at(home.path()).unwrap();

        assert_eq!(
            fs::read_to_string(&legacy_path).unwrap(),
            "#!/bin/bash\necho pre\necho post\n"
        );
        assert_eq!(
            fs::read_to_string(hook_path_from_home(home.path())).unwrap(),
            HOOK_CONTENT
        );
    }

    #[test]
    fn installation_does_not_rewrite_an_unowned_legacy_hook() {
        let home = TestHome::new();
        let legacy_path = legacy_hook_path_from_home(home.path());
        let original = "#!/bin/bash\n\n  echo custom  \n\n";
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(&legacy_path, original).unwrap();

        install_hook_at(home.path()).unwrap();

        assert_eq!(fs::read_to_string(legacy_path).unwrap(), original);
    }

    #[test]
    fn uninstall_removes_only_rgbpc_owned_hooks() {
        let home = TestHome::new();
        let legacy_path = legacy_hook_path_from_home(home.path());
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(&legacy_path, "#!/bin/bash\necho custom\n").unwrap();
        install_hook_at(home.path()).unwrap();

        remove_hook_at(home.path()).unwrap();

        assert!(!hook_path_from_home(home.path()).exists());
        assert_eq!(
            fs::read_to_string(legacy_path).unwrap(),
            "#!/bin/bash\necho custom\n"
        );
    }

    #[test]
    fn restore_autostart_path_uses_xdg_autostart_dir() {
        let path = get_restore_autostart_path().unwrap();
        assert!(path.ends_with(".config/autostart/rgbpc-restore.desktop"));
    }
}
