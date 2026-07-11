import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Project Pages: https://harkirat155.github.io/peekdb/
export default defineConfig({
  plugins: [react()],
  base: "/peekdb/",
});
