import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The board binary serves the built `dist/` at `/`; in dev we proxy the API
// (including the SSE stream) to it on its default port.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": {
        target: "http://127.0.0.1:4242",
        changeOrigin: true,
      },
    },
  },
});
