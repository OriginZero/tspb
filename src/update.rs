//! `tspb update`: replace this binary with a release from GitHub.
//!
//! Downloads go through the direct GitHub URL first and fall back to public
//! GitHub mirrors, which makes the update usable from networks where
//! `github.com` / `objects.githubusercontent.com` are slow or blocked.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

const REPO: &str = "OriginZero/tspb";
const API_LATEST: &str = "https://api.github.com/repos/OriginZero/tspb/releases/latest";

/// Public GitHub download proxies. Every entry is prepended verbatim to the
/// real URL, so `MIRROR + https://github.com/...` is the proxied address.
/// `TSPB_MIRROR` (or `--mirror`) replaces this list at runtime.
const MIRRORS: &[&str] = &[
    "https://ghfast.top/",
    "https://gh-proxy.com/",
    "https://ghproxy.net/",
    "https://hub.gitmirror.com/",
];

pub struct Options {
    pub version: Option<String>,
    pub mirror: Option<String>,
}

pub fn run(opts: Options) -> Result<()> {
    let target = target_triple()?;
    let current = env!("CARGO_PKG_VERSION");
    println!("tspb v{current} ({target})");

    let mirror = opts
        .mirror
        .or_else(|| std::env::var("TSPB_MIRROR").ok())
        .filter(|m| !m.trim().is_empty())
        .map(|m| if m.ends_with('/') { m } else { format!("{m}/") });

    let tag = match opts.version {
        Some(v) => normalize_tag(&v),
        None => {
            let latest = latest_tag(mirror.as_deref())?;
            if parse_version(&latest) <= parse_version(current) {
                println!("tspb v{current} is already the latest release.");
                return Ok(());
            }
            println!("latest release: {latest}");
            latest
        }
    };

    let asset = format!("tspb-{tag}-{target}.{}", archive_ext());
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let url = format!("{base}/{asset}");

    let tmp = std::env::temp_dir().join(format!("tspb-update-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let result = install_from(&tmp, &url, &asset, &base, mirror.as_deref());
    let _ = std::fs::remove_dir_all(&tmp);
    result?;

    println!("updated to {tag}. restart tspb to use it.");
    Ok(())
}

fn install_from(
    tmp: &Path,
    url: &str,
    asset: &str,
    base: &str,
    mirror: Option<&str>,
) -> Result<()> {
    let archive = tmp.join(asset);
    fetch(url, &archive, mirror).with_context(|| format!("downloading {url}"))?;

    if let Some(expected) = expected_sha256(&format!("{base}/SHA256SUMS.txt"), asset, mirror) {
        let actual = sha256_hex(&archive)?;
        if actual != expected {
            bail!("checksum mismatch for {asset}: expected {expected}, got {actual}");
        }
        println!("checksum verified");
    }

    extract(&archive, tmp)?;
    let extracted = tmp.join(if cfg!(windows) { "tspb.exe" } else { "tspb" });
    if !extracted.exists() {
        bail!("{} does not contain the tspb binary", asset);
    }
    replace_current_exe(&extracted)
}

fn target_triple() -> Result<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok("x86_64-pc-windows-msvc")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Ok("aarch64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok("x86_64-apple-darwin")
    } else {
        bail!("no prebuilt release for this platform; please build from source")
    }
}

fn archive_ext() -> &'static str {
    if cfg!(windows) { "zip" } else { "tar.gz" }
}

fn normalize_tag(raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with('v') {
        raw.to_string()
    } else {
        format!("v{raw}")
    }
}

fn parse_version(v: &str) -> Vec<u64> {
    v.trim_start_matches('v')
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn latest_tag(mirror: Option<&str>) -> Result<String> {
    let mut urls = vec![API_LATEST.to_string()];
    if let Some(m) = mirror {
        urls.insert(0, format!("{m}{API_LATEST}"));
    }
    let mut last = None;
    for url in urls {
        match fetch_text(&url) {
            Ok(body) => {
                if let Some(tag) = tag_from_json(&body) {
                    return Ok(tag);
                }
                last = Some(format!("{url} returned no tag_name"));
            }
            Err(e) => last = Some(format!("{url}: {e}")),
        }
    }
    bail!(
        "cannot resolve the latest release ({}). Retry with 'tspb update --version vX.Y.Z'",
        last.unwrap_or_else(|| "no response".into())
    )
}

fn tag_from_json(body: &str) -> Option<String> {
    let key = "\"tag_name\"";
    let rest = &body[body.find(key)? + key.len()..];
    let rest = &rest[rest.find('"')? + 1..];
    Some(rest[..rest.find('"')?].to_string())
}

/// Every download is tried directly first, then through each mirror unless the
/// user pinned one with `--mirror` / `TSPB_MIRROR` (then only that one is used).
fn candidates(url: &str, mirror: Option<&str>) -> Vec<String> {
    match mirror {
        Some(m) => vec![format!("{m}{url}")],
        None => {
            let mut all = vec![url.to_string()];
            all.extend(MIRRORS.iter().map(|m| format!("{m}{url}")));
            all
        }
    }
}

fn fetch(url: &str, dest: &Path, mirror: Option<&str>) -> Result<()> {
    let mut errors = Vec::new();
    for candidate in candidates(url, mirror) {
        match download(&candidate, dest, false) {
            Ok(()) => return Ok(()),
            Err(e) => errors.push(format!("{candidate}: {e}")),
        }
    }
    bail!("{}", errors.join("; "))
}

fn download(url: &str, dest: &Path, quiet: bool) -> Result<()> {
    use std::process::Stdio;
    let dest_str = dest.to_string_lossy().into_owned();
    let attempts: [(&str, Vec<&str>); 2] = [
        (
            "curl",
            vec![
                "-fsSL",
                "--retry",
                "2",
                "--connect-timeout",
                "10",
                "--max-time",
                "300",
                "-o",
                &dest_str,
                url,
            ],
        ),
        ("wget", vec!["-q", "-O", &dest_str, url]),
    ];
    let mut last = None;
    for (program, args) in attempts {
        let mut cmd = Command::new(program);
        cmd.args(&args);
        if quiet {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
        match cmd.status() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => last = Some(format!("{program} exited with {status}")),
            Err(e) => last = Some(format!("{program}: {e}")),
        }
    }
    bail!("{}", last.unwrap_or_else(|| "no downloader".into()))
}

fn fetch_text(url: &str) -> Result<String> {
    for (program, args) in [
        ("curl", vec!["-fsSL", "--max-time", "20", url]),
        ("wget", vec!["-qO-", url]),
    ] {
        if let Ok(output) = Command::new(program).args(&args).output()
            && output.status.success()
        {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
    }
    bail!("curl and wget both failed")
}

/// `SHA256SUMS.txt` is missing on older releases; verification is best effort.
fn expected_sha256(sums_url: &str, asset: &str, mirror: Option<&str>) -> Option<String> {
    let tmp = std::env::temp_dir().join(format!("tspb-sums-{}", std::process::id()));
    let fetched = candidates(sums_url, mirror)
        .iter()
        .any(|url| download(url, &tmp, true).is_ok());
    if !fetched {
        return None;
    }
    let body = std::fs::read_to_string(&tmp).ok()?;
    let _ = std::fs::remove_file(&tmp);
    body.lines()
        .filter(|line| line.ends_with(asset))
        .filter_map(|line| line.split_whitespace().next())
        .map(|hash| hash.to_ascii_lowercase())
        .next()
}

fn sha256_hex(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let status = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .context("running tar; extract the archive manually if it is unavailable")?;
    if !status.success() {
        bail!("tar could not extract {}", archive.display());
    }
    Ok(())
}

#[cfg(windows)]
fn replace_current_exe(new: &Path) -> Result<()> {
    let current = std::env::current_exe()?;
    // Windows may rename a running executable but not overwrite it, so move the
    // old binary aside first; the leftover `.old` file is removed on next update.
    let old = std::path::PathBuf::from(format!("{}.old", current.display()));
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&current, &old)?;
    if let Err(e) = std::fs::copy(new, &current) {
        let _ = std::fs::rename(&old, &current);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(unix)]
fn replace_current_exe(new: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let current = std::env::current_exe()?;
    let staged = current.with_extension("new");
    std::fs::copy(new, &staged)?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    // Renaming over the running binary is atomic and safe on Unix.
    std::fs::rename(&staged, &current)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_parsing_and_version_ordering() {
        let body = r#"{"tag_name": "v0.2.1", "name": "v0.2.1"}"#;
        assert_eq!(tag_from_json(body).unwrap(), "v0.2.1");
        assert!(parse_version("v0.2.1") > parse_version("v0.1.9"));
        assert!(parse_version("v0.10.0") > parse_version("v0.9.9"));
        assert_eq!(normalize_tag("1.2.3"), "v1.2.3");
        assert_eq!(normalize_tag("v1.2.3"), "v1.2.3");
        assert_eq!(parse_version("v0.1.0-rc1"), vec![0, 1, 0, 0]);
    }

    #[test]
    fn mirror_prefixes_wrap_the_original_url() {
        let url = "https://github.com/x/y/releases/download/v1/f.tar.gz";
        assert_eq!(
            candidates(url, Some("https://mirror/")).first().unwrap(),
            "https://mirror/https://github.com/x/y/releases/download/v1/f.tar.gz"
        );
        assert_eq!(candidates(url, None).len(), MIRRORS.len() + 1);
    }
}
