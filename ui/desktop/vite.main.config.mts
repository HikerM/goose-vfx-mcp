import { defineConfig } from 'vite';

// https://vitejs.dev/config
export default defineConfig({
  define: {
    'process.env.LUMINA_RELEASE_OWNER': JSON.stringify(process.env.LUMINA_RELEASE_OWNER || ''),
    'process.env.LUMINA_RELEASE_REPO': JSON.stringify(process.env.LUMINA_RELEASE_REPO || ''),
    'process.env.LUMINA_HOMEPAGE': JSON.stringify(process.env.LUMINA_HOMEPAGE || ''),
    'process.env.LUMINA_DISTRIBUTION_MODE': JSON.stringify(
      process.env.LUMINA_DISTRIBUTION_MODE || 'portable'
    ),
    'process.env.LUMINA_BUNDLE_NAME': JSON.stringify(process.env.LUMINA_BUNDLE_NAME || 'Lumina'),
  },
});
