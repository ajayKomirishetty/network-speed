pub mod parser;
pub mod runner;

use std::path::PathBuf;

/// Resolve the iperf3 executable to use, in priority order:
///
/// 1. An explicit custom path (must exist as a file).
/// 2. A bundled copy sitting next to the application executable
///    (this is how the Windows installer ships it).
/// 3. The first `iperf3` / `iperf3.exe` found on `PATH`.
pub fn resolve_iperf3(custom: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = custom {
        let path = PathBuf::from(path.trim());
        if !path.as_os_str().is_empty() && path.is_file() {
            return Some(path);
        }
    }

    let binary_name = if cfg!(windows) {
        "iperf3.exe"
    } else {
        "iperf3"
    };

    // Bundled copy shipped by the installer.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // PATH lookup.
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(binary_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}
