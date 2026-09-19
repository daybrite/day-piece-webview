// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

use std::path::Path;

/// WebView2's folder mapping needs a DOS/UNC path, not the extended-length spelling returned
/// by Windows `canonicalize`. Passing `\\?\D:\...` through to its local-content loader gives
/// the page ERR_INVALID_URL even though the directory exists and the mapping call can succeed.
pub(crate) fn mapping_folder(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    if let Some(disk) = text.strip_prefix(r"\\?\") {
        let bytes = disk.as_bytes();
        if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b":\\" {
            return disk.to_owned();
        }
    }
    text.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_drive_folder_becomes_a_dos_path() {
        assert_eq!(
            mapping_folder(Path::new(
                r"\\?\D:\a\day-piece-lottie\demo\build\day\assets"
            )),
            r"D:\a\day-piece-lottie\demo\build\day\assets"
        );
        assert_eq!(
            mapping_folder(Path::new(r"\\?\C:\Users\Zoë\Day demo\assets")),
            r"C:\Users\Zoë\Day demo\assets"
        );
    }

    #[test]
    fn canonical_unc_folder_keeps_its_server_and_share() {
        assert_eq!(
            mapping_folder(Path::new(r"\\?\UNC\server\share\assets")),
            r"\\server\share\assets"
        );
    }

    #[test]
    fn other_path_spellings_are_not_reinterpreted() {
        for path in [
            r"C:\Program Files\Day\assets",
            r"\\server\share\assets",
            r"assets",
            r"\\?\Volume{1234}\assets",
            r"\\.\device",
        ] {
            assert_eq!(mapping_folder(Path::new(path)), path);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_canonical_directory_still_resolves_after_conversion() {
        let canonical = std::env::temp_dir().canonicalize().unwrap();
        assert!(canonical.to_string_lossy().starts_with(r"\\?\"));
        let folder = mapping_folder(&canonical);
        assert!(!folder.starts_with(r"\\?\"));
        assert_eq!(Path::new(&folder).canonicalize().unwrap(), canonical);
    }
}
