import { readdirSync } from "node:fs";
import tailwind from "@tailwindcss/vite";
import { defineConfig } from "vite";

// Every document here is an entry: index.html is the report, ipc.html is the
// one served with a Content-Security-Policy to find out whether Tauri's IPC
// survives it. Read rather than listed, because a document someone adds is
// exactly the one a hand-written list would leave unbuilt.
const documents = Object.fromEntries(
  readdirSync(import.meta.dirname)
    .filter((name) => name.endsWith(".html"))
    .toSorted()
    .map((name) => [name.replace(/\.html$/, ""), name]),
);

// Vite marks the tags it injects `crossorigin`, which makes the webview fetch
// them in CORS mode. That is harmless while a custom scheme yields a real
// origin, and fatal the moment one yields an opaque origin: the page would
// simply never load, and a diagnostic tool that fails silently is worse than
// none. Whether a scheme is opaque is one of the things being measured here, so
// the page must not depend on the answer.
const withoutCrossorigin = {
  name: "probe-without-crossorigin",
  transformIndexHtml: (html: string) => html.replaceAll(" crossorigin", ""),
};

// Tailwind is here for its reset and nothing else - `probe.css` imports
// `preflight.css` and never `tailwindcss`, so no utility engine runs and no
// class name in the markup means anything to it. The plugin is still required:
// preflight is written in Tailwind's own `--theme()` dialect, and without
// something to compile it the browser would drop the declarations that use it.
export default defineConfig({
  plugins: [tailwind(), withoutCrossorigin],
  // Unminified, and named the same way on every build. The probe prints a line
  // per request, and a log full of `index-Ba7f2c.js` says less than one full of
  // `index.js`: the bytes the webview received should be legible to whoever is
  // reading why a platform behaved the way it did.
  build: {
    outDir: "dist",
    emptyOutDir: true,
    minify: false,
    // current floor is macOS with Safari 17.4
    target: "es2024",
    rollupOptions: {
      input: documents,
      output: {
        entryFileNames: "assets/[name].js",
        chunkFileNames: "assets/[name].js",
        assetFileNames: "assets/[name][extname]",
      },
    },
  },
});
