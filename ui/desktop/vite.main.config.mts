import { defineConfig } from 'vite';

// https://vitejs.dev/config
export default defineConfig({
  define: {
    'process.env.GITHUB_OWNER': JSON.stringify('HikerM'),
    'process.env.GITHUB_REPO': JSON.stringify('goose-vfx-mcp'),
    'process.env.GOOSE_BUNDLE_NAME': JSON.stringify(process.env.GOOSE_BUNDLE_NAME || 'Goose'),
  },
});
