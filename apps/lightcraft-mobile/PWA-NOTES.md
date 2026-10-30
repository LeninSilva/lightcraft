# LightCraft mobile PWA notes

Installability for the mobile web build: manifest, service worker,
and icons, all referenced with relative paths so the app works at a
domain root and under a subpath.

## Files (all under apps/lightcraft-mobile/)

- `manifest.webmanifest`: name/short_name "LightCraft", display
  "standalone", background/theme `#141414`, start_url "." and scope
  "." (relative).
- `sw.js`: precaches the app shell only (index.html, the JS glue,
  the wasm module, the manifest, the three icons) under the versioned
  cache `lightcraft-mobile-v1`, serves those files cache-first, and
  passes everything else through to the network. User photos and
  library data live in OPFS/IndexedDB and are never fetched over
  HTTP, so the worker never sees or stores them. Old caches are
  deleted on activate.
- `icons/icon-192.png`, `icons/icon-512.png` (maskable),
  `icons/apple-touch-icon.png` (180x180).
- `index.html`: manifest link, theme-color, mobile-web-app-capable,
  apple-mobile-web-app-capable/title, apple-touch-icon link, and a
  small registration script that registers `./sw.js` with scope
  `./` only on https or localhost and fails silently. The loading
  overlay, panic hook, and `?debug=1` probe are untouched.

## Icons: original work

The icons are original, generated in code, not copied or traced from
any upstream or third-party asset. Generator: a short Python/Pillow
(PIL 10.2.0) script that draws, on a full-bleed `#141414` background,
a warm amber (`#e8a33d`) camera-aperture mark: an outer ring, six
tangential blade lines spiralling to an inner hexagon, and a small
soft-white centre dot. The glyph sits inside the central 80% of the
frame so maskable cropping cannot clip it. No font is used; no logo
asset from the LightCraft repository or any other project is included.

## Build-pipeline note (fixed)

`cargo xtask mobile` (xtask/src/mobile.rs) originally copied only `index.html`
from apps/lightcraft-mobile/ into the site output. That is now fixed in the same
commit as these PWA files: `STATIC_FILES` includes `manifest.webmanifest` and
`sw.js`, and `ICON_FILES` copies the three icons into `icons/` in the bundle.
The service worker is not registered inside the Android shell (Capacitor):
the shell already serves local assets, and a cache-first worker there could
keep serving the previous build after an APK update.
