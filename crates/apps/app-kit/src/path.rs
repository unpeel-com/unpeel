use std::path::Path;

/// Render `path` from a project's root when it is inside that root.
///
/// The root itself is `.`. Paths outside the root stay unchanged rather than
/// acquiring misleading `..` components. This is display-only: semantic drag
/// maps and filesystem operations should continue using absolute paths.
#[must_use]
pub fn display_path_from_root(path: impl AsRef<Path>, root: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    let root = root.as_ref();
    match path.strip_prefix(root) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
        Ok(relative) => relative.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_inside_a_project_are_root_relative() {
        assert_eq!(
            display_path_from_root("/work/unpeel/apps/native/App.swift", "/work/unpeel"),
            "apps/native/App.swift"
        );
        assert_eq!(display_path_from_root("/work/unpeel", "/work/unpeel"), ".");
    }

    #[test]
    fn component_boundaries_and_outsiders_stay_absolute() {
        assert_eq!(
            display_path_from_root("/work/unpeel-other/file", "/work/unpeel"),
            "/work/unpeel-other/file"
        );
        assert_eq!(
            display_path_from_root("/tmp/file", "/work/unpeel"),
            "/tmp/file"
        );
    }
}
