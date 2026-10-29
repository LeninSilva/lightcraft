//! `cargo xtask mobile`: build `apps/lightcraft-mobile` for wasm32 and bundle it with
//! `wasm-bindgen` into `<target>/mobile-site/` (index.html + lightcraft_mobile.js +
//! lightcraft_mobile_bg.wasm), with gzip and brotli precompressed copies (`*.gz`, `*.br`)
//! next to each file. The bundle is what the Capacitor APK workflow
//! (`.github/workflows/build-mobile-apk.yml`) copies into `www/`; the workflow deletes the
//! precompressed copies before `cap sync`, because Android's asset merger treats
//! `foo.js` and `foo.js.gz` as duplicate resources.
//!
//! This mirrors `web.rs` (`cargo xtask web`) deliberately: same target, same profile, same
//! `wasm-bindgen` version check against Cargo.lock, same compressors. The bindgen/compression
//! helpers are duplicated here rather than shared with `web.rs` so a change to the desktop
//! web bundle cannot silently change the mobile bundle. Keep them in sync by hand.
//!
//! Differences from the web build: no render workers (`worker.js`), no separately shipped
//! Chinese font (the mobile app uses eframe's default fonts), and no `--serve` mode; serve
//! the folder with any static HTTP server for a browser smoke test.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{cargo, metadata, root, run as step};

const TARGET: &str = "wasm32-unknown-unknown";
/// Cargo profile for the optimized (`--dev` off) build; see `[profile.web]` in `Cargo.toml`.
const WEB_PROFILE: &str = "web";
/// Directory under `<target>` that receives the bundle. It must **not** be a Cargo profile name:
/// cross-compiling with `--target wasm32-unknown-unknown` makes Cargo write the selected profile's
/// *host* artifacts to `<target>/<profile>/` (see `web.rs`).
const SITE_DIR: &str = "mobile-site";

/// Files copied from `apps/lightcraft-mobile/` into the bundle as they are.
const STATIC_FILES: [&str; 1] = ["index.html"];

/// Bundle files that get precompressed copies.
const COMPRESSED: [&str; 3] = ["index.html", "lightcraft_mobile.js", "lightcraft_mobile_bg.wasm"];

/// The `wasm-bindgen` version pinned in Cargo.lock (the CLI must match it exactly).
fn locked_bindgen_version() -> Result<String, String> {
    let lock = std::fs::read_to_string(root().join("Cargo.lock")).map_err(|e| format!("Cargo.lock: {e}"))?;
    let mut lines = lock.lines();
    while let Some(l) = lines.next() {
        if l.trim() == "name = \"wasm-bindgen\""
            && let Some(v) = lines.next().and_then(|v| v.trim().strip_prefix("version = \""))
        {
            return Ok(v.trim_end_matches('"').to_string());
        }
    }
    Err("wasm-bindgen not found in Cargo.lock".into())
}

fn check_bindgen(want: &str) -> Result<(), String> {
    let hint = format!("install it with:\n    cargo install wasm-bindgen-cli --version {want} --locked");
    let out = Command::new("wasm-bindgen").arg("--version").output().map_err(|_| format!("`wasm-bindgen` CLI not found; {hint}"))?;
    let have = String::from_utf8_lossy(&out.stdout);
    let have = have.split_whitespace().nth(1).unwrap_or("?");
    if have != want {
        return Err(format!("wasm-bindgen CLI is {have} but Cargo.lock has {want}; {hint}"));
    }
    Ok(())
}

pub fn run(args: &[&str]) -> Result<(), String> {
    let dev = args.contains(&"--dev");
    if let Some(bad) = args.iter().find(|a| *a != "--dev") {
        return Err(format!("unknown argument `{bad}` (only `--dev` is supported; serve the output with any static HTTP server)"));
    }

    let want = locked_bindgen_version()?;
    check_bindgen(&want)?;
    let meta = metadata()?;
    let target_dir = PathBuf::from(meta["target_directory"].as_str().ok_or("cargo metadata: no target_directory")?);
    let profile = if dev { "dev" } else { WEB_PROFILE };

    let mut c = cargo();
    c.args(["build", "-p", "lightcraft-mobile", "--lib", "--target", TARGET, "--profile", profile]);
    step(c, &format!("cargo build -p lightcraft-mobile --target {TARGET} --profile {profile}"))?;

    let wasm = target_dir.join(TARGET).join(if dev { "debug" } else { profile }).join("lightcraft_mobile.wasm");
    let out = target_dir.join(SITE_DIR);
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut b = Command::new("wasm-bindgen");
    b.arg(&wasm).args(["--target", "web", "--no-typescript", "--out-name", "lightcraft_mobile", "--out-dir"]).arg(&out);
    if dev {
        b.arg("--debug");
    }
    step(b, &format!("wasm-bindgen {} → {}", wasm.display(), out.display()))?;

    // optional size pass when binaryen is installed
    let bg = out.join("lightcraft_mobile_bg.wasm");
    if !dev && Command::new("wasm-opt").arg("--version").output().is_ok() {
        let mut o = Command::new("wasm-opt");
        o.args(["-O2", "--enable-bulk-memory", "--enable-nontrapping-float-to-int", "--enable-sign-ext", "--enable-mutable-globals"])
            .arg(&bg)
            .arg("-o")
            .arg(&bg);
        if step(o, "wasm-opt -O2").is_err() {
            eprintln!("(wasm-opt failed; keeping the unoptimized module)");
        }
    }
    for f in STATIC_FILES {
        std::fs::copy(root().join("apps/lightcraft-mobile").join(f), out.join(f)).map_err(|e| format!("copy {f}: {e}"))?;
    }

    // precompressed copies (skipped for --dev: they'd only slow the edit loop down).
    // The APK workflow deletes them before `cap sync`; a browser host may serve them.
    println!("\n{:<26} {:>12} {:>12} {:>12}", "file", "bytes", "gzip -9", "brotli -11");
    for f in COMPRESSED {
        let p = out.join(f);
        let raw = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let (gz, br) = if dev {
            for ext in ["gz", "br"] {
                let _ = std::fs::remove_file(out.join(format!("{f}.{ext}")));
            }
            (None, None)
        } else {
            let gz = gzip(&raw)?;
            let br = brotli(&raw)?;
            std::fs::write(out.join(format!("{f}.gz")), &gz).map_err(|e| format!("{f}.gz: {e}"))?;
            std::fs::write(out.join(format!("{f}.br")), &br).map_err(|e| format!("{f}.br: {e}"))?;
            (Some(gz.len()), Some(br.len()))
        };
        let show = |n: Option<usize>| n.map_or("-".to_string(), |n| n.to_string());
        println!("{f:<26} {:>12} {:>12} {:>12}", raw.len(), show(gz), show(br));
    }
    let size = std::fs::metadata(&bg).map(|m| m.len()).unwrap_or(0);
    println!("\nmobile build ready: {} ({:.1} MB wasm)", out.display(), size as f64 / 1e6);
    println!("serve it with any static HTTP server and open http://127.0.0.1:8080/ (it needs HTTP, not file://)");
    Ok(())
}

fn gzip(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).and_then(|_| e.finish()).map_err(|e| format!("gzip: {e}"))
}

fn brotli(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let params = ::brotli::enc::BrotliEncoderParams { quality: 11, lgwin: 24, ..Default::default() };
    ::brotli::BrotliCompress(&mut &data[..], &mut out, &params).map_err(|e| format!("brotli: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_dir_is_not_a_cargo_profile() {
        // Same reason as web.rs: the bundle directory must not be a profile directory, or
        // cross-compiling would drop host artifacts into it.
        let manifest = std::fs::read_to_string(root().join("Cargo.toml")).unwrap();
        let is_profile = |name: &str| {
            let header = format!("[profile.{name}]");
            manifest.lines().any(|l| l.trim() == header)
        };
        assert!(is_profile(WEB_PROFILE), "`{WEB_PROFILE}` is no longer a profile in Cargo.toml");
        assert!(!is_profile(SITE_DIR), "`{SITE_DIR}` is a Cargo profile name; pick a different bundle directory");
        assert_ne!(SITE_DIR, "web-site", "the mobile bundle must not overwrite the desktop web bundle");
    }

    #[test]
    fn bundle_names_match_the_entry_point() {
        // apps/lightcraft-mobile/index.html imports ./lightcraft_mobile.js, which loads
        // lightcraft_mobile_bg.wasm; the xtask must emit exactly those names.
        assert!(COMPRESSED.contains(&"lightcraft_mobile.js"));
        assert!(COMPRESSED.contains(&"lightcraft_mobile_bg.wasm"));
        assert!(STATIC_FILES.contains(&"index.html"));
    }
}
