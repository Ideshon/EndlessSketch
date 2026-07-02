use std::path::{Path, PathBuf};

const LOCAL_DATA_DIR: &str = "local";
const DEFAULT_CANVAS_DIR: &str = "default.esketch";
const SETTINGS_FILE: &str = "settings.json";

pub fn local_data_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .map(|path| local_data_root_from_exe(&path))
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|path| path.join(LOCAL_DATA_DIR))
        })
        .unwrap_or_else(|| PathBuf::from(LOCAL_DATA_DIR))
}

pub fn local_data_root_from_exe(exe_path: &Path) -> PathBuf {
    exe_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(LOCAL_DATA_DIR)
}

pub fn default_canvas_path() -> PathBuf {
    local_data_root().join(DEFAULT_CANVAS_DIR)
}

pub fn default_canvas_path_from_root(local_root: &Path) -> PathBuf {
    local_root.join(DEFAULT_CANVAS_DIR)
}

pub fn settings_path() -> PathBuf {
    settings_path_from_root(&local_data_root())
}

pub fn settings_path_from_root(local_root: &Path) -> PathBuf {
    local_root.join(SETTINGS_FILE)
}

#[cfg(test)]
mod tests {
    use super::{default_canvas_path_from_root, local_data_root_from_exe, settings_path_from_root};
    use std::path::Path;

    #[test]
    fn local_data_root_is_next_to_the_executable() {
        let root = local_data_root_from_exe(Path::new(r"C:\Apps\EndlessSketch\endless-sketch.exe"));

        assert_eq!(root, Path::new(r"C:\Apps\EndlessSketch\local"));
    }

    #[test]
    fn local_paths_are_derived_from_the_local_root() {
        let root = Path::new(r"C:\Apps\EndlessSketch\local");

        assert_eq!(
            default_canvas_path_from_root(root),
            Path::new(r"C:\Apps\EndlessSketch\local\default.esketch")
        );
        assert_eq!(
            settings_path_from_root(root),
            Path::new(r"C:\Apps\EndlessSketch\local\settings.json")
        );
    }
}
