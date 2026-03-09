use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const HOOK_BEGIN: &str = "# >>> rgbpc sync hook >>>";
const HOOK_END: &str = "# <<< rgbpc sync hook <<<";
const HOOK_BLOCK: &str = r#"# >>> rgbpc sync hook >>>
rgbpc --sync-theme &
# <<< rgbpc sync hook <<<"#;

pub fn get_hook_path() -> PathBuf {
    let mut path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    path.push(".config/omarchy/hooks/theme-set");
    path
}

pub fn is_hook_installed() -> bool {
    let path = get_hook_path();
    if let Ok(content) = fs::read_to_string(&path) {
        content.contains(HOOK_BEGIN) && content.contains(HOOK_END)
    } else {
        false
    }
}

pub fn install_hook() -> Result<(), String> {
    let path = get_hook_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut content = remove_managed_block(&existing).trim_end().to_string();

    if content.is_empty() {
        content = format!("#!/bin/bash\n{}\n", HOOK_BLOCK);
    } else {
        if !content.starts_with("#!") {
            content = format!("#!/bin/bash\n{}", content);
        }
        content.push_str("\n\n");
        content.push_str(HOOK_BLOCK);
        content.push('\n');
    }

    fs::write(&path, content).map_err(|e| e.to_string())?;

    let mut perms = fs::metadata(&path)
        .map_err(|e| e.to_string())?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).map_err(|e| e.to_string())?;

    Ok(())
}

pub fn remove_hook() -> Result<(), String> {
    let path = get_hook_path();
    if !path.exists() {
        return Ok(());
    }

    let existing = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let updated = remove_managed_block(&existing).trim().to_string();

    if updated.is_empty() || updated == "#!/bin/bash" {
        fs::remove_file(path).map_err(|e| e.to_string())?;
        return Ok(());
    }

    fs::write(&path, format!("{}\n", updated)).map_err(|e| e.to_string())?;
    Ok(())
}

fn remove_managed_block(content: &str) -> String {
    if let Some(start) = content.find(HOOK_BEGIN) {
        if let Some(end_rel) = content[start..].find(HOOK_END) {
            let end = start + end_rel + HOOK_END.len();
            let mut updated = String::new();
            updated.push_str(&content[..start]);
            updated.push_str(&content[end..]);
            return updated.replace("\n\n\n", "\n\n");
        }
    }

    content.to_string()
}

#[cfg(test)]
mod tests {
    use super::remove_managed_block;

    #[test]
    fn removes_only_rgbpc_managed_block() {
        let content = "#!/bin/bash\necho pre\n# >>> rgbpc sync hook >>>\nrgbpc --sync-theme &\n# <<< rgbpc sync hook <<<\necho post\n";
        let updated = remove_managed_block(content);
        assert!(updated.contains("echo pre"));
        assert!(updated.contains("echo post"));
        assert!(!updated.contains("rgbpc --sync-theme"));
    }
}
