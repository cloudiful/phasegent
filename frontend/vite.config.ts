import { fileURLToPath, URL } from 'node:url'
import { defineConfig, type Plugin } from 'vite'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'
import ui from '@nuxt/ui/vite'

const frontendRoot = fileURLToPath(new URL('./', import.meta.url))

// Vite tags generated assets with `crossorigin`, which makes WebKit fetch them
// in CORS mode. Tauri's custom scheme serves the bundle without CORS headers,
// so a CORS-mode stylesheet fetch fails and the GUI renders unstyled, while
// module scripts (which genuinely require CORS) still load. Strip the
// attribute from rel=stylesheet links only, after Vite injects them.
function stylesheetWithoutCrossorigin(): Plugin {
  return {
    name: 'phasegent:stylesheet-without-crossorigin',
    enforce: 'post',
    transformIndexHtml: {
      order: 'post',
      handler(html) {
        return html.replace(/<link\b[^>]*>/gi, (tag) => {
          const rel = tag.match(/\brel\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/i)
          const relValue = rel?.[1] ?? rel?.[2] ?? rel?.[3] ?? ''
          if (!relValue.toLowerCase().split(/\s+/).includes('stylesheet')) return tag
          return tag.replace(/\s+crossorigin\b(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+))?/i, '')
        })
      },
    },
  }
}

export default defineConfig({
  root: frontendRoot,
  // Relative asset URLs so the bundle also loads from Tauri's file-based
  // static host (frontendDist) without server rewrites.
  base: './',
  plugins: [
    vue(),
    tailwindcss(),
    ui({
      experimental: { componentDetection: true },
    }),
    stylesheetWithoutCrossorigin(),
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
