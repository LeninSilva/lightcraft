# LightCraft Mobile: platform notes

The touch-first app in `apps/lightcraft-mobile/` (branch
`claude/lightcraft-android-apk-nk14le`, HEAD `203ef7e` at the time this was written).
Build it with `cargo xtask mobile` (output: `target/mobile-site/`); the APK workflow
is `.github/workflows/build-mobile-apk.yml` (artifact `lightcraft-mobile-apk`, debug
only, sideload only, app ID `ai.storyteller.lightcraft`). Nothing in this piece touches
the desktop-port workflow on branch `android-apk` or its artifact
`lightcraft-android-apk`.

This file states exactly what the platform glue implements, what is stubbed, and what
must be tested on a phone. Merged with this note: the touch UI in
`apps/lightcraft-mobile/src/ui/mod.rs`, the analysis/looks/advisor implementation,
and this platform glue, all uncompiled locally at merge time. The workflow runs
the native tests before the wasm build, so a compile or test failure there is
the first check.

## Implemented

- `cargo xtask mobile` (`xtask/src/mobile.rs`, wired in `xtask/src/main.rs`): builds
  `lightcraft-mobile` for `wasm32-unknown-unknown` with the `[profile.web]` profile,
  checks the `wasm-bindgen` CLI against the version pinned in `Cargo.lock` (0.2.129),
  runs `wasm-bindgen --target web --no-typescript --out-name lightcraft_mobile`,
  applies the optional `wasm-opt -O2` size pass, copies `index.html`, and writes
  gzip/brotli precompressed copies of the three bundle files. `--dev` builds the
  `dev` profile and removes stale precompressed copies. No `--serve` mode.
- WASM entry point (`apps/lightcraft-mobile/src/web.rs`): `start()` mounts the app on
  `<canvas id="lightcraft_canvas">` via `eframe::WebRunner`, using this egui fork's
  split `logic`/`ui` trait (mirroring `apps/lightcraft-web`'s `WebApp`). The page is
  `apps/lightcraft-mobile/index.html` (same canvas id, `touch-action: none`,
  viewport locked so page pinch-zoom does not fight canvas gestures, no worker, no
  separate font fetch).
- Host boundary, browser side (`apps/lightcraft-mobile/src/ui/host.rs`, `WebHost`,
  wasm32 only): `PickPhoto` opens the browser file picker (single file,
  `image/*` plus common photo/raw extensions) and answers `Picked`/`PickCancelled`;
  `Put`/`Get`/`Delete` go to browser storage (below) and answer `Loaded`/`Failed`;
  `SaveToDevice` triggers a browser download (JPEG/PNG MIME by extension, other
  names get `application/octet-stream`) and answers `Saved` with location
  `Downloads/<name>` or `Failed`. Storage requests sent before storage opens are
  queued and flushed in order; if storage fails to open they answer `Failed`.
  `now_ms` is `js_sys::Date::now()`.
- Browser storage (`apps/lightcraft-mobile/src/ui/storage.rs`, wasm32 only): OPFS
  when writable from the main thread, else IndexedDB, adapted from
  `apps/lightcraft-web/src/backend.rs` (whole-file read/write/remove only). The
  IndexedDB database is `lightcraft-mobile`, separate from the desktop web app's
  `lightcraft` database. Storage keys are the slash paths named by `library.rs`
  (`library/index.json`, `library/original/<id>`, `library/version/<id>`, ...).
- Host boundary, native side: `MemoryHost` is unchanged, and `safe_file_name`
  (download-name sanitising) plus a `MemoryHost` round-trip have native tests in
  `host.rs`, so the boundary stays testable without a browser.

## Stubbed / not implemented

- `Share` in `WebHost` always answers `Failed` ("not available in this build yet;
  save the file first"). No Capacitor Share plugin and no Web Share API call.
- No `?store=` URL override (the desktop web app has `?store=idb`/`?store=memory`).
  Storage is always OPFS-first, IndexedDB fallback.
- No persistent-storage request (`navigator.storage.persist()`), no tab lock, no
  render workers: preview renders run on the main thread and block the UI while
  they run. The draft-while-dragging / full-on-release rendering split is a UI
  piece, not done here.
- No native (non-WASM) host: on a desktop build the crate has no entry point, by
  design. Native runs use `MemoryHost` in tests only.
- No signing, no Play Store release: debug APK only, as in the desktop-port build.

## Assumptions made (verify on the phone / in CI)

- The crate builds to `lightcraft_mobile.wasm` (package `lightcraft-mobile`, `--lib`,
  `cdylib`), and the `web-sys` feature names added to
  `apps/lightcraft-mobile/Cargo.toml` exist in web-sys 0.3.106. The feature list was
  copied from `apps/lightcraft-web`, which uses the same APIs; it was not compiled
  here (no Rust toolchain in this VM).
- The egui fork's `eframe::App` trait provides `update` from `logic`/`ui`, as the
  desktop web app's `WebApp` relies on; `MobileApp` implements `logic` (empty) and
  `ui` (calls `App::frame`).
- The Capacitor steps (`cap init`, `cap add android`, `capacitor-assets generate`,
  Kotlin stdlib excludes, `cap sync`) behave identically for this bundle as they
  do for the desktop-port bundle, since the shell, app ID, name, icon, and asset
  layout are the same. The workflow triggers on pushes to
  `claude/lightcraft-android-apk-nk14le`; if the branch is merged or renamed, the
  trigger branch must change with it.

## Must be phone-tested (not verified here)

1. Install the `lightcraft-mobile-apk` artifact; the app opens to the LightCraft
   screen with no blank canvas or load error.
2. Import: pick a photo from the phone gallery; it appears in the library and
   survives closing and reopening the app (OPFS/IndexedDB persistence).
3. Slider: dragging a tune control updates the preview without freezing the app
   for seconds; releasing shows the full preview.
4. Export: saving a version downloads a full-resolution JPEG the phone's Photos/
   Files app can open, at the expected size.
5. Persistence: after reopening, kept photos and saved versions are still listed,
   and let-go photos are gone under the retention policy in `library.rs`.
6. Download in the Capacitor WebView: confirm the export actually lands in
   Downloads (the anchor-download path is proven in a desktop browser, not in
   this WebView shell).
