import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  base: '/ui/',
  plugins: [vue()],
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
