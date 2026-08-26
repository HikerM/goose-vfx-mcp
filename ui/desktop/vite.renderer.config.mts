import { defineConfig } from 'vite';
import tailwindcss from '@tailwindcss/vite';

// https://vitejs.dev/config
export default defineConfig({
  define: {
    'process.env.LUMINA_TUNNEL': JSON.stringify(
      process.env.LUMINA_TUNNEL !== 'no' && process.env.LUMINA_TUNNEL !== 'none'
    ),
    'process.env.LUMINA_RELEASE_OWNER': JSON.stringify(process.env.LUMINA_RELEASE_OWNER || ''),
    'process.env.LUMINA_RELEASE_REPO': JSON.stringify(process.env.LUMINA_RELEASE_REPO || ''),
    'process.env.LUMINA_HOMEPAGE': JSON.stringify(process.env.LUMINA_HOMEPAGE || ''),
  },

  plugins: [tailwindcss()],

  // Vite caches a copy of @hikerm/lumina-sdk and doesn't notice when we rebuild it
  // locally, so it serves stale code until you clear node_modules/.vite by hand.
  // Excluding it makes Vite always read the latest ui/sdk/dist build.
  // Dev-server only — release builds ignore optimizeDeps.
  optimizeDeps: {
    exclude: ['@hikerm/lumina-sdk'],
  },

  build: {
    target: 'esnext',
  },
});
