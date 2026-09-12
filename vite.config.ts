import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;
// GitHub Pages serves only the public demo from the repository subpath.
// Normal WebUI and Tauri builds intentionally keep Vite's root base.
// @ts-expect-error process is a nodejs global
const base = process.env.VITE_PAGES_DEMO === "1" ? "/workbuddy-switch/" : "/";

// https://vite.dev/config/
export default defineConfig(async () => ({
  base,
  plugins: [react(), tailwindcss()],
  // esbuild 0.28.2 minify 在 React 19 下会产生坏产物（useRef/useState null 白屏）
  build: {
    minify: false,
    // rollup 对 react 的 CJS interop 生成了两个实例（react_production/$1），
    // 部分组件（App.tsx/UpdateCenter）绑到无 dispatcher 的副本 → hooks null 白屏。
    // strictRequires 强制每个 CJS 模块严格惰性包装，消除重复实例。
    commonjsOptions: {
      strictRequires: true,
      transformMixedEsModules: true,
    },
  },
  resolve: {
    alias: [
      { find: "@", replacement: path.resolve(__dirname, "./src") },
      // 收敛 React 单实例：产物曾出现两份 react_production（hooks dispatcher
      // 落空 → useRef null → 白屏）。精确正则 alias 到唯一物理入口文件。
      { find: /^react$/, replacement: path.resolve(__dirname, "node_modules/react/index.js") },
      { find: /^react-dom$/, replacement: path.resolve(__dirname, "node_modules/react-dom/index.js") },
      { find: /^react\//, replacement: path.resolve(__dirname, "node_modules/react") + "/" },
      { find: /^react-dom\//, replacement: path.resolve(__dirname, "node_modules/react-dom") + "/" },
    ],
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    // 本机坑：默认 localhost 只绑 IPv6 ::1，Tauri WebView2 走 IPv4 会白壳。
    // 显式监听全部接口（含 127.0.0.1），devUrl http://localhost:1420 双栈可达。
    host: host || true,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
