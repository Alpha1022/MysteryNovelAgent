import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 要求固定端口；clearScreen:false 保留 Rust 编译输出
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    host: '0.0.0.0',
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    target: "es2021",
  },
});
