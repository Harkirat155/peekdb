import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Project Pages: https://harkirat155.github.io/peekdb/
export default defineConfig({
  plugins: [react()],
  base: "/peekdb/",
  resolve: {
    // Preact-compat: much smaller runtime than React for this marketing shell
    alias: {
      react: "preact/compat",
      "react-dom": "preact/compat",
      "react/jsx-runtime": "preact/jsx-runtime",
      "react-dom/client": "preact/compat/client",
    },
  },
  build: {
    target: "es2020",
    cssMinify: true,
  },
});
