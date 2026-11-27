//! Locates the Wobbly Life `Save` directory.

use std::path::{Path, PathBuf};

#[cfg(not(windows))]
const APP_ID: &str = "1211020";
const SAVE_SUBPATH: &[&str] = &["AppData", "LocalLow", "RubberBandGames", "Wobbly Life", "Save"];

/// Returns every existing `Save` directory found by the platform search rules.
pub fn save_dirs() -> Vec<PathBuf> {
    // Canonical paths detect duplicates (symlinked Steam roots); the original
    // paths are returned because Windows canonical paths carry a `\\?\` prefix.
    let mut found: Vec<(PathBuf, PathBuf)> = Vec::new();
    for c in candidates() {
        if c.is_dir() {
            let canon = c.canonicalize().unwrap_or_else(|_| c.clone());
            if !found.iter().any(|(k, _)| *k == canon) {
                found.push((canon, c));
            }
        }
    }
    found.into_iter().map(|(_, p)| p).collect()
}

#[cfg(windows)]
fn candidates() -> Vec<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(|home| vec![join_all(PathBuf::from(home), SAVE_SUBPATH)])
        .unwrap_or_default()
}

/// Proton prefix inside every Steam library listed in `libraryfolders.vdf`.
#[cfg(not(windows))]
fn candidates() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let steam_roots = [
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".steam/root"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        home.join("snap/steam/common/.local/share/Steam"),
    ];
    let mut libraries: Vec<PathBuf> = Vec::new();
    for root in &steam_roots {
        libraries.push(root.clone());
        if let Ok(vdf) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
            libraries.extend(library_paths(&vdf));
        }
    }
    libraries
        .into_iter()
        .map(|lib| {
            let prefix = lib
                .join("steamapps/compatdata")
                .join(APP_ID)
                .join("pfx/drive_c/users/steamuser");
            join_all(prefix, SAVE_SUBPATH)
        })
        .collect()
}

fn join_all(mut base: PathBuf, parts: &[&str]) -> PathBuf {
    base.extend(parts);
    base
}

#[cfg(not(windows))]
/// Extracts the values of `"path"` keys from a Steam `libraryfolders.vdf`.
fn library_paths(vdf: &str) -> impl Iterator<Item = PathBuf> + '_ {
    vdf.lines().filter_map(|line| {
        let rest = line.trim().strip_prefix("\"path\"")?.trim();
        let value = rest.strip_prefix('"')?.strip_suffix('"')?;
        Some(PathBuf::from(value.replace("\\\\", "\\")))
    })
}

/// Account directories (named by 64-bit Steam ID) inside a `Save` directory.
pub fn account_dirs(save_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(save_dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    out.sort();
    Ok(out)
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn parses_library_paths() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"/mnt/gamez/SteamLibrary\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t}\n}\n";
        let v: Vec<_> = library_paths(vdf).collect();
        assert_eq!(v, [PathBuf::from("/mnt/gamez/SteamLibrary"), PathBuf::from("D:\\SteamLibrary")]);
    }
}
