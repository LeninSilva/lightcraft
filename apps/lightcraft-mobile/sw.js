// LightCraft mobile service worker: app-shell precache only.
//
// Strategy: on install, precache exactly the app shell (page, script,
// wasm module, manifest, icons) under a versioned cache name, and serve
// those shell files cache-first on fetch. Everything else (in
// particular user photos and library data, which live in
// OPFS/IndexedDB and are never fetched over HTTP) is passed straight
// through to the network and never stored here. On activate, caches
// from older versions are deleted. All URLs are relative so the worker
// works at a domain root and under a subpath, with scope "./".
// Registration (see index.html) only happens on https or localhost
// and fails silently, so a failed install never shows error UI.

const CACHE = "lightcraft-mobile-v1";

const SHELL = [
  "./",
  "./index.html",
  "./lightcraft_mobile.js",
  "./lightcraft_mobile_bg.wasm",
  "./manifest.webmanifest",
  "./icons/icon-192.png",
  "./icons/icon-512.png",
  "./icons/apple-touch-icon.png",
];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.addAll(SHELL))
      .then(() => self.skipWaiting())
      .catch(() => {
        // Precache failure must not break the page: the app still loads
        // from the network. The error stays in the console only.
      }),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(
          keys.filter((key) => key !== CACHE).map((key) => caches.delete(key)),
        ),
      )
      .then(() => self.clients.claim())
      .catch(() => {}),
  );
});

self.addEventListener("fetch", (event) => {
  // Only same-origin GETs for the shell are served from cache. Anything
  // else (including navigation to a photo or any non-GET) goes to the
  // network untouched, so user data is never cached by this worker.
  if (event.request.method !== "GET") return;
  const url = new URL(event.request.url);
  if (url.origin !== self.location.origin) return;
  // Match on pathname only: a query string (e.g. the app's ?debug=1
  // probe) must not knock a shell file out of the cache.
  const dir = self.location.pathname.replace(/[^/]*$/, "");
  const path = url.pathname;
  const shell = SHELL.some((entry) => {
    if (entry === "./") {
      // The start URL: the scope root, with or without index.html.
      return path === dir || path === dir + "index.html";
    }
    return path === dir + entry.slice(2);
  });
  if (!shell) return;
  event.respondWith(
    caches.match(event.request, { ignoreSearch: true }).then((hit) => {
      if (hit) return hit;
      return fetch(event.request);
    }),
  );
});
