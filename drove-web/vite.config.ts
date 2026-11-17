import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";

const apiProxy = {
  "/api": {
    target: "http://127.0.0.1:7799",
    changeOrigin: true,
    timeout: 0,
    proxyTimeout: 0,
  },
};

/** Dev server only. The herdr2 daemon injects the token when it serves dist. */
function injectDevToken(): Plugin {
  return {
    name: "drove-dev-token",
    apply: "serve",
    transformIndexHtml(html) {
      const stateHome = process.env.XDG_STATE_HOME || join(homedir(), ".local", "state");
      const session = process.env.H2_SESSION || "default";
      let token = "";
      try {
        const info = JSON.parse(readFileSync(join(stateHome, "herdr2", session, "drove", "web.info.json"), "utf8")) as { token?: unknown };
        if (typeof info.token === "string") token = info.token;
      } catch {
        return html;
      }
      if (!token) return html;
      const encoded = JSON.stringify(token).replaceAll("<", "\\u003c");
      const script = `<script>window.__DROVE_TOKEN__=${encoded};window.__CO_TOKEN__=${encoded};</script>`;
      return html.includes("</head>") ? html.replace("</head>", `${script}</head>`) : `${script}${html}`;
    },
  };
}

export default defineConfig({
  plugins: [react(), injectDevToken()],
  server: { port: 5173, proxy: apiProxy },
  preview: { port: 4173, proxy: apiProxy },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
