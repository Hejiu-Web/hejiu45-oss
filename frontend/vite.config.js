import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// The console is embedded into the Rust binary at compile time, so the build
// output must be small and self-contained: no CDN links, no remote fonts, and
// asset file names are hashed for immutable caching.
export default defineConfig({
  plugins: [vue()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'es2020',
    assetsInlineLimit: 8192,
  },
  server: {
    port: 5173,
    proxy: {
      // Development only: `cargo run` serves the API on 9000.
      '/api': {
        target: 'http://127.0.0.1:9000',
        changeOrigin: false,
      },
    },
  },
})
