//! Locate a Terraria installation without asking the user.

use std::path::{Path, PathBuf};

/// Search the usual Steam and GOG locations for a Terraria install.
pub fn find_terraria() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    let home = home_dir();

    let mut steam_roots = Vec::new();
    if let Some(home) = &home {
        steam_roots.push(home.join(".local/share/Steam"));
        steam_roots.push(home.join(".steam/steam"));
        steam_roots.push(home.join(".steam/root"));
        steam_roots.push(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
        steam_roots.push(home.join("snap/steam/common/.local/share/Steam"));
        steam_roots.push(home.join("Library/Application Support/Steam"));
    }
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        steam_roots.push(PathBuf::from(data_home).join("Steam"));
    }
    #[cfg(windows)]
    {
        if let Some(program_files) = std::env::var_os("ProgramFiles(x86)") {
            steam_roots.push(PathBuf::from(program_files).join("Steam"));
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            steam_roots.push(PathBuf::from(program_files).join("Steam"));
        }
    }

    for root in &steam_roots {
        candidates.push(root.join("steamapps/common/Terraria"));
        // Terraria may live in a second (or third) Steam library.
        for library in steam_libraries(root) {
            candidates.push(library.join("steamapps/common/Terraria"));
        }
    }

    if let Some(home) = &home {
        candidates.push(home.join("GOG Games/Terraria"));
        candidates.push(home.join("Games/Terraria"));
        candidates.push(home.join("Terraria"));
    }
    #[cfg(windows)]
    {
        if let Some(program_files) = std::env::var_os("ProgramFiles(x86)") {
            candidates.push(PathBuf::from(program_files).join("GOG Galaxy/Games/Terraria"));
        }
    }

    candidates
        .into_iter()
        .find(|candidate| is_terraria(candidate))
}

/// A Terraria install always contains `Content` with the game assets.
pub fn is_terraria(path: &Path) -> bool {
    path.join("Content").is_dir()
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

fn steam_libraries(steam_root: &Path) -> Vec<PathBuf> {
    let path = steam_root.join("steamapps/libraryfolders.vdf");
    match std::fs::read_to_string(path) {
        Ok(text) => parse_library_folders(&text),
        Err(_) => Vec::new(),
    }
}

/// Parse `steamapps/libraryfolders.vdf`.
///
/// Supports the current format (`"path" "/some/dir"`) and the older one
/// (`"1" "/some/dir"`).
fn parse_library_folders(text: &str) -> Vec<PathBuf> {
    let mut libraries = Vec::new();

    for line in text.lines() {
        let line = line.trim();

        if let Some(rest) = line.strip_prefix("\"path\"") {
            if let Some((value, _)) = take_quoted(rest.trim_start()) {
                libraries.push(PathBuf::from(unescape(value)));
            }
            continue;
        }

        if let Some((key, rest)) = take_quoted(line)
            && key.parse::<u32>().is_ok()
            && let Some((value, _)) = take_quoted(rest.trim_start())
            && looks_like_path(value)
        {
            libraries.push(PathBuf::from(unescape(value)));
        }
    }

    libraries
}

fn take_quoted(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('"')?;
    let end = text.find('"')?;
    Some((&text[..end], &text[end + 1..]))
}

fn looks_like_path(value: &str) -> bool {
    value.starts_with('/') || value.starts_with('\\') || value.contains(':')
}

fn unescape(value: &str) -> String {
    value.replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_and_legacy_library_formats() {
        let text = r#"
"libraryfolders"
{
    "0"
    {
        "path"      "/home/player/.local/share/Steam"
        "label"     ""
        "apps"
        {
            "105600"    "123"
        }
    }
    "1"
    {
        "path"      "D:\\SteamLibrary"
    }
    "2"             "/mnt/games/SteamLibrary"
}
"#;

        assert_eq!(
            parse_library_folders(text),
            vec![
                PathBuf::from("/home/player/.local/share/Steam"),
                PathBuf::from("D:\\SteamLibrary"),
                PathBuf::from("/mnt/games/SteamLibrary"),
            ]
        );
    }

    #[test]
    fn ignores_unrelated_quoted_values() {
        let text = r#"
"libraryfolders"
{
    "0"
    {
        "path"      "/games/steam"
        "label"     "not a path"
    }
}
"#;
        assert_eq!(
            parse_library_folders(text),
            vec![PathBuf::from("/games/steam")]
        );
    }
}
