// Applies the saved theme before first paint (no light flash in dark mode). Loaded as a
// blocking external script because the app CSP forbids inline scripts. Keep in sync with
// src/lib/settings.svelte.ts, which takes over once the app has started.
(() => {
  let theme = localStorage.getItem("stv.theme");
  if (theme !== "light" && theme !== "dark") {
    theme = window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  document.documentElement.dataset.theme = theme;
})();
