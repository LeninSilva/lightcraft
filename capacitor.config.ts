import type { CapacitorConfig } from '@capacitor/cli';

// Committed Capacitor configuration for the LightCraft mobile shell.
// The workflow must NOT run `npx cap init`: init merges/rewrites this file
// depending on the CLI version and whether TypeScript resolves, so the
// workflow uses `npx cap add android` + `npx cap sync` against this
// committed config instead. appId/appName are written into the native
// project once, during `cap add`; later edits here do not change them.
const config: CapacitorConfig = {
  appId: 'ai.storyteller.lightcraft',
  appName: 'LightCraft',
  webDir: 'www',
  server: {
    androidScheme: 'https',
  },
  plugins: {
    SplashScreen: {
      launchShowDuration: 2000,
      launchAutoHide: true,
      launchFadeOutDuration: 500,
      backgroundColor: '#1c1c1c',
      androidSplashResourceName: 'splash',
      androidScaleType: 'CENTER_CROP',
      showSpinner: false,
      splashFullScreen: true,
      splashImmersive: false,
    },
  },
};

export default config;
