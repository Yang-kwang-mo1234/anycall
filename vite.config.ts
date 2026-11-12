import { defineConfig } from "vite";

// Tauri 개발 서버 설정 (https://v2.tauri.app/start/frontend/vite/)
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
