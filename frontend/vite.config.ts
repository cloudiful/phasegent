import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'
import ui from '@nuxt/ui/vite'

const frontendRoot = fileURLToPath(new URL('./', import.meta.url))

export default defineConfig({
  root: frontendRoot,
  // Relative asset URLs so the bundle also loads from Tauri's file-based
  // static host (frontendDist) without server rewrites.
  base: './',
  plugins: [
    vue(),
    tailwindcss(),
    ui(),
  ],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    chunkSizeWarningLimit: 1200,
  },
  server: {
    port: 1420,
    strictPort: true,
  },
})
