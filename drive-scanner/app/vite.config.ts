import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';

// Tauri loads the built files through its own protocol in the release app (no localhost).
// The dev server below is only used by `tauri dev` on a developer machine.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: 'chrome120', outDir: 'dist', sourcemap: false },
});
