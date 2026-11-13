use anyhow::{Context, Result};
use std::path::PathBuf;

/// Write text into the OS clipboard.
///
/// Windows/macOS hand ownership to the OS, so the handle can be dropped.
/// On X11 the `Clipboard` object itself serves selection requests; dropping it
/// would discard the text we just published, so it is kept alive until exit.
pub fn set_text(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new().context("open system clipboard")?;
    clipboard
        .set_text(text.to_owned())
        .context("write system clipboard")?;
    #[cfg(target_os = "linux")]
    std::mem::forget(clipboard);
    Ok(())
}

/// Headless / remote sessions may have no clipboard owner. Keep the yanked text
/// reachable instead of silently discarding it.
pub fn fallback_file(text: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join("tspb-clipboard.txt");
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}
