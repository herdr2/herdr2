# co web

编排图前端。数据来自本机 daemon 的 `/api`（见 `docs/TECH_SPEC.md` §5、§11）。页面不提供直接回复操作，那些操作留在宿主里用 `co reply` / `co ack` 完成。

## 开发

先在另一个终端启动 daemon（默认 `127.0.0.1:7788`），再起前端：

```sh
cd web
npm install
npm run dev
```

浏览器打开 `http://127.0.0.1:5173`。Vite 把 `/api`（含 SSE `/api/events`）代理到 `http://127.0.0.1:7788`，代理不设超时，避免长连接被掐断。

写操作（改派、停止 worker）需要请求头 `x-co-token`。开发服务器会读取 `$CO_HOME/daemon.json`（未设置 `CO_HOME` 时是 `~/.co/daemon.json`）并把 token 注入页面。验收和测试请使用临时 `CO_HOME`，不要往用户的 `~/.co` 里写。

不连 daemon 时打开 `http://127.0.0.1:5173/?mock=1`，使用内置假数据。

## 构建

```sh
cd web
npm run build
```

`tsc -b` 通过后，Vite 把产物写到仓库的 `dist/web`（`vite.config.ts` 的 `build.outDir`）。

按 TECH_SPEC，daemon 应托管这个目录：`GET /` 和 `/assets/*` 返回 `dist/web`，并把 `window.__CO_TOKEN__` 写进 `index.html`。然后打开 `http://127.0.0.1:7788`。

当前 `createDaemon` 的默认 `staticDir` 是仓库里的 `web/`（源码），不是 `dist/web`。`co daemon start` 直接打开 7788 时，浏览器拿到的是 Vite 源码入口，不会执行构建后的包。要用构建结果，启动时传入 `staticDir` 指向 `dist/web`。`npm run preview` 在 4173 端口预览构建结果，同样把 `/api` 代理到 7788。
