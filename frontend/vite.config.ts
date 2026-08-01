import { defineConfig } from 'vite'
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'

export default defineConfig({
  base: '/ui/',
  plugins: [vue(), tailwindcss()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  server: {
    host: '0.0.0.0',
    port: Number(process.env.VITE_PORT || 8090),
    strictPort: true,
    allowedHosts: true,
    proxy: {
      '/api': process.env.VITE_API_PROXY_TARGET || 'http://127.0.0.1:18090',
    },
  },
})
