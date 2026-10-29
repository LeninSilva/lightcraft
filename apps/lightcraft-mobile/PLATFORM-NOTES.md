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
- Share (`WebHost`, wasm32 only): the Web Share API with files. When the browser
  has no share API or cannot share files, the file is downloaded instead and the
  answer is `Saved` (Downloads). A cancelled or rejected share answers `Failed`
  with the real reason ("Sharing was cancelled." / "Sharing failed (…)."). The
  decision helpers are pure and tested natively in `host.rs`.
- Browser storage requests persistence best effort (`navigator.storage.persist()`,
  mirroring the desktop web app) when the backend opens; a refusal never fails
  opening. See `storage.rs`.
- Composition (`recipe.rs`): `blend()` now interpolates grading hues along the
  shortest arc with the strength (previously they snapped to the look's hue at
  any strength above zero), plus grading blending/balance, grain size/roughness,
  vignette shape and the remaining detail fields; the focus mask radius is
  converted from a long-edge fraction to normalized radii so the focus area is
  round on non-square photos; the Fade curve is clamped to the tone range; and
  black-and-white keeps both saturation and vibrance at or below zero. Preview,
  look thumbnails and export all use the same `compose()` path (thumbnails render
  the small proxy, the preview the working copy; grain is resolution-scaled by
  the pipeline, so the two agree).

## Stubbed / not implemented

- No `?store=` URL override (the desktop web app has `?store=idb`/`?store=memory`).
  Storage is always OPFS-first, IndexedDB fallback.
- No tab lock: two tabs could open the same OPFS store. Porting the desktop
  app's tab lock needs its single-tab semantics and is left for later; phones
  normally run one instance.
- No render workers: preview renders run on the main thread and block the UI
  while they run. Workers need a `worker.js` plus a message protocol ported from
  the desktop web app; the draft-while-dragging / full-on-release split in the
  UI already limits the cost, and is the reason workers were not added here.
- No tap-to-place subject and no pinch/double-tap zoom: both need gesture wiring
  in the UI, which was out of scope for this integration pass. The UI states
  both plainly in the Focus and Crop panels.
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
7. Share: tapping Share opens the system share sheet with the full-resolution
   JPEG; in a WebView without file sharing, the file downloads instead. Cancelling
   the sheet shows "Could not share: Sharing was cancelled.", not a generic error.

## Ordering and recovery (bug-fix pass, uncompiled locally)

- Storage requests run through one sequential worker in `WebHost`, in queue
  order, so a stale index write cannot land after a newer one. Requests sent
  before the backend opens wait in the same queue.
- The library index is parsed strictly: a corrupt stored index keeps the
  in-memory library, tries the backup copy (`library/index-backup.json`,
  written alongside every good index), and blocks index writes until a
  readable index loads, so a corrupt index is never overwritten with an
  empty one.
- Add photo has no picker-open guard (a cancelled browser pick fires no
  event, which used to deadlock it); the picker answers `PickCancelled` on
  every failure path that can produce one.
- Export/share file names carry the photo id, version number and export
  time, so repeated exports do not overwrite each other in Downloads.
- A failed full-resolution decode is an export error, never a silent
  low-resolution proxy export.
