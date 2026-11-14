//! Folder-scoped context shared by the desktop and background scanner.
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

pub type FolderContexts = BTreeMap<PathBuf, String>;

pub fn folder_key(path: &Path) -> PathBuf {
    if let Ok(path) = path.canonicalize() {
        return path;
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut normalized = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            part => normalized.push(part.as_os_str()),
        }
    }
    normalized
}

pub fn resolve(
    folder: &Path,
    saved: &FolderContexts,
    profiles: &[crate::watch::Folder],
) -> (String, bool) {
    let folder = folder_key(folder);
    let explicit = saved
        .iter()
        .filter(|(path, _)| folder.starts_with(path))
        .max_by_key(|(path, _)| path.components().count());
    let profile = profiles
        .iter()
        .filter(|profile| {
            let path = folder_key(&profile.path);
            !profile.hint.is_empty()
                && (folder == path || profile.recursive && folder.starts_with(path))
        })
        .max_by_key(|profile| profile.path.components().count());
    if let Some((path, text)) = explicit
        && profile.is_none_or(|profile| {
            path.components().count() >= folder_key(&profile.path).components().count()
        })
    {
        return (text.clone(), false);
    }
    if let Some(profile) = profile {
        return (profile.hint.clone(), false);
    }
    (infer(&folder), true)
}

/// Use meaningful directory names, skipping capture containers and dated subfolders.
pub fn infer(folder: &Path) -> String {
    let home = directories::BaseDirs::new().map(|base| base.home_dir().to_owned());
    for path in folder.ancestors() {
        if home.as_deref() == Some(path) {
            break;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            break;
        };
        if name.starts_with('.') {
            break;
        }
        let mut cleaned = String::new();
        let mut previous = ' ';
        for ch in name.chars() {
            if (ch.is_uppercase() && previous.is_lowercase())
                || (ch.is_ascii_digit() && previous.is_alphabetic())
            {
                cleaned.push(' ');
            }
            cleaned.push(if ch.is_alphanumeric() { ch } else { ' ' });
            previous = ch;
        }
        let mut words: Vec<_> = cleaned.split_whitespace().collect();
        fn generic(word: &str) -> bool {
            matches!(
                word.to_ascii_lowercase().as_str(),
                "clips"
                    | "clip"
                    | "captures"
                    | "capture"
                    | "recordings"
                    | "recording"
                    | "highlights"
                    | "replays"
                    | "replay"
                    | "exports"
                    | "videos"
                    | "video"
                    | "obs"
                    | "gameplay"
                    | "games"
            )
        }
        while words.first().is_some_and(|word| generic(word)) {
            words.remove(0);
        }
        while words.last().is_some_and(|word| generic(word)) {
            words.pop();
        }
        // Dates at the beginning are capture dates; keep numbers belonging to game names.
        while words
            .first()
            .is_some_and(|word| word.chars().all(|ch| ch.is_ascii_digit()))
        {
            words.remove(0);
        }
        let text = words.join(" ");
        if matches!(
            text.to_ascii_lowercase().as_str(),
            "home" | "users" | "mnt" | "media" | "tmp" | "var" | "storage" | "recordings"
        ) {
            break;
        }
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guesses_game_names_and_skips_containers_and_dates() {
        for (path, expected) in [
            ("/recordings/Hunt__Showdown_1896", "Hunt Showdown 1896"),
            ("/recordings/Helldivers2_Clips", "Helldivers 2"),
            (
                "/captures/Hunt_Showdown_1896/2026-11-14",
                "Hunt Showdown 1896",
            ),
            (
                "/recordings/2026-11-14_Hunt_Showdown_1896",
                "Hunt Showdown 1896",
            ),
            ("/clips", ""),
            ("/tmp/.temp/recordings", ""),
        ] {
            assert_eq!(infer(Path::new(path)), expected, "{path}");
        }
    }
    #[test]
    fn saved_context_inherits_and_empty_override_stays_empty() {
        let mut saved = FolderContexts::new();
        saved.insert(
            folder_key(Path::new("/clips/Hunt")),
            "Custom PvP context".into(),
        );
        assert_eq!(
            resolve(Path::new("/clips/Hunt/day"), &saved, &[]),
            ("Custom PvP context".into(), false)
        );
        saved.insert(folder_key(Path::new("/clips/Hunt/day")), String::new());
        assert_eq!(
            resolve(Path::new("/clips/Hunt/day"), &saved, &[]),
            (String::new(), false)
        );
        assert_eq!(
            resolve(Path::new("/clips/Helldivers2"), &saved, &[]).0,
            "Helldivers 2"
        );
    }
}
