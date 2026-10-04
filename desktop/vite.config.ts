import { defineConfig } from "vite"
import solid from "vite-plugin-solid"
import { fileURLToPath, URL } from "node:url"

export default defineConfig({
  plugins: [solid()],
  resolve: {
    alias: { "@": fileURLToPath(new URL("../src", import.meta.url)) },
  },
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "127.0.0.1", fs: { allow: [".."] } },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: { target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13" },
})
