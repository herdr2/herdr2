---
name: drove-setup
description: Configure Drove Roles (`Agent`, `Model`, `Thinking Effort`), the minimal AI Assistant (`Service URL` + `API Key`), and optional Magpie Quota visibility. Guides users to `Herdr2 App -> Settings -> Drove` and verifies probe/connectivity via `h2 drove`.
disable-model-invocation: true
---

# `/drove-setup`: Configure Drove Roles & Services

In `herdr2`, Drove configuration lives in **`Herdr2 App -> Settings -> Drove`** (shared across local and SSH-connected remote machines via `--on <machine>`):

## 1. Recommended UI Setup (Interactive)

Ask the user to open **`Herdr2 App -> Settings -> Drove`** (or trigger `app.settings {"section":"drove"}`):
1. **Target Machine**: Select `This computer` or any configured SSH machine.
2. **Refresh Agents**: Click **Refresh Agents** (`h2 drove probe --refresh`) to discover installed ACP CLIs (`agy`, `claude`, `codex`, `gemini`, `opencode`, `qwen`, `dsh`) and their live model/effort options.
3. **Roles**: Assign `Agent`, `Model`, and `Thinking Effort` (`low` / `medium` / `high` / `xhigh`) plus optional Fallback per Role (`coder`, `tester`, `writer`, `reviewer`, `researcher`, or custom roles).
4. **AI Assistant (Optional)**: Enter `Service URL` + `API Key` (`0600` in `secrets/ai.key`), click **Save**, then **Test Connection**.
5. **Magpie Quota (Optional)**: Disabled by default; toggle ON only if using a local Magpie gateway.

## 2. CLI Verification & Diagnostics

```bash
h2 drove probe --refresh [--on <machine>]
h2 drove settings [--on <machine>]
h2 drove ai-test [--url https://api.example.com/v1] [--on <machine>]
h2 drove status [--on <machine>]
```

For updating settings non-interactively with an API key without exposing secrets on process `argv`, write a temporary JSON payload and pass `--consume`:

```bash
h2 drove settings-set --file /tmp/drove-cfg.json --consume
```
