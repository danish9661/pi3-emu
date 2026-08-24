import { defineConfig } from 'vite';

// Cross-Origin Isolation (COI) headers. The qemu-wasm Linux engine uses
// SharedArrayBuffer (pthread build), which requires the page to be
// cross-origin isolated. A same-origin <iframe> can only be isolated when its
// parent is too, so we isolate the whole app here. The per-page
// coi-serviceworker.js in public/linux/ then sees crossOriginIsolated already
// true and becomes a no-op (it remains the fallback for static hosts that
// don't send these headers).
const COI_HEADERS = {
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
};

function coiHeadersPlugin() {
  const set = (req, res, next) => {
    for (const k in COI_HEADERS) res.setHeader(k, COI_HEADERS[k]);
    next();
  };
  return {
    name: 'coi-headers',
    configureServer(server) {
      server.middlewares.use(set);
    },
    configurePreviewServer(server) {
      server.middlewares.use(set);
    },
  };
}

export default defineConfig({
  base: './',
  plugins: [coiHeadersPlugin()],
});
