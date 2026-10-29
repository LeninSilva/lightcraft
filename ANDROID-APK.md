# LightCraft Android APK (fork build)

This fork adds one thing only to upstream LightCraft 0.15.0: a GitHub Actions
workflow (`.github/workflows/build-android-apk.yml`) that builds the
WebAssembly web app and bundles it offline in a Capacitor Android shell.
The app ID is `ai.storyteller.lightcraft`, chosen by Lenin Silva.
The workflow runs on pushes to the `android-apk` branch and on manual dispatch.
It uploads a debug APK artifact named `lightcraft-android-apk`.
A debug APK is sideload only. It is not signed for the Play Store.

## Fork-only UI changes

Three small changes in `crates/ui-egui`, so dead controls do not fail on tap:

1. The Forward icon in the top bar was removed. There is no `view.forward`
   command, so it did nothing on any platform.
2. Presets panel Import and Export are now disabled when the host has no file
   dialog, which is the case on the web. Before, they failed with a
   no-dialog error.
3. The Object and Describe mask tiles show a dimmed icon and a toast saying
   AI masks are not available in this build, when the SAM feature is off.
   The web build never enables it.

No other upstream behavior was changed. No upstream bug was claimed fixed.

## What works, and what does not, in the APK

The APK runs the same web build upstream ships. The editing core works:
sliders, Tone Curve, Color Mixer, Color Grading, crop, brush masks,
Subject/Sky/Background heuristic masks, applying presets, and export
as a download.

These do not work in the APK, because the web build does not include them:

- Object and Describe AI masks (SAM 3 model, not in the web build).
- AI Denoise (no denoise runtime or models in the web build).
- Print, Contact Sheet, and Slideshow Export (no save dialog on the web).
- Preset, curve, keyword, and mask effect file import and export
  (no file dialogs wired on the web).
- Show in Finder, Open in External Editor, Add Folder, and Lightroom
  catalog import (desktop only).

Touch limits: the UI is the desktop UI. Right click menus, hover previews,
double click resets, and modifier key actions have no touch equivalent.
Export saves through the browser download path inside the WebView.
The library lives in WebView storage for this app only. Keep originals
elsewhere and use File, Back Up Library, before clearing app data.

## Install

Download the `lightcraft-android-apk` artifact from the Actions run,
extract the APK, allow install from that source on the phone, and install it.
