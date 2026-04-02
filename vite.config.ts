import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import UnoCSS from "@unocss/vite";

export default defineConfig({
  plugins: [UnoCSS(), svelte()],
  server: {
    host: "127.0.0.1",
    port: 5173,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    outDir: "dist",
  },
});
