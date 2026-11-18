// Language detection, curated open-source IDE/Markdown themes, and Markdown helpers for Files.

const MARKDOWN_EXTS = new Set(["md", "markdown", "mdx", "mdown", "mkd"]);
const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff"]);

const EXACT_FILES = {
  dockerfile: { language: "dockerfile", label: "Dockerfile" },
  containerfile: { language: "dockerfile", label: "Containerfile" },
  makefile: { language: "makefile", label: "Makefile" },
  gnumakefile: { language: "makefile", label: "Makefile" },
  justfile: { language: "sh", label: "Justfile" },
  "cmakelists.txt": { language: "cmake", label: "CMake" },
  "cargo.lock": { language: "toml", label: "TOML" },
  ".gitignore": { language: "ini", label: "Git Ignore" },
  ".gitattributes": { language: "ini", label: "Git Attributes" },
  ".dockerignore": { language: "ini", label: "Docker Ignore" },
  ".editorconfig": { language: "ini", label: "EditorConfig" },
  ".env": { language: "ini", label: "Env" },
  ".bashrc": { language: "sh", label: "Bash" },
  ".zshrc": { language: "sh", label: "Zsh" },
};

const EXT_LANGS = {
  rs: { language: "rs", label: "Rust" },
  ts: { language: "ts", label: "TypeScript" },
  mts: { language: "ts", label: "TypeScript" },
  cts: { language: "ts", label: "TypeScript" },
  tsx: { language: "tsx", label: "TSX" },
  js: { language: "js", label: "JavaScript" },
  mjs: { language: "js", label: "JavaScript" },
  cjs: { language: "js", label: "JavaScript" },
  jsx: { language: "jsx", label: "JSX" },
  py: { language: "py", label: "Python" },
  pyi: { language: "py", label: "Python" },
  go: { language: "go", label: "Go" },
  c: { language: "c", label: "C" },
  h: { language: "c", label: "C Header" },
  cc: { language: "cpp", label: "C++" },
  cpp: { language: "cpp", label: "C++" },
  cxx: { language: "cpp", label: "C++" },
  hpp: { language: "cpp", label: "C++ Header" },
  hh: { language: "cpp", label: "C++ Header" },
  cs: { language: "cs", label: "C#" },
  java: { language: "java", label: "Java" },
  kt: { language: "kt", label: "Kotlin" },
  kts: { language: "kt", label: "Kotlin" },
  scala: { language: "scala", label: "Scala" },
  swift: { language: "swift", label: "Swift" },
  rb: { language: "rb", label: "Ruby" },
  php: { language: "php", label: "PHP" },
  sh: { language: "sh", label: "Shell" },
  bash: { language: "bash", label: "Bash" },
  zsh: { language: "zsh", label: "Zsh" },
  fish: { language: "fish", label: "Fish" },
  ps1: { language: "ps1", label: "PowerShell" },
  psm1: { language: "ps1", label: "PowerShell" },
  bat: { language: "bat", label: "Batch" },
  cmd: { language: "bat", label: "Batch" },
  json: { language: "json", label: "JSON" },
  jsonc: { language: "jsonc", label: "JSONC" },
  jsonl: { language: "jsonc", label: "JSONL" },
  toml: { language: "toml", label: "TOML" },
  yaml: { language: "yaml", label: "YAML" },
  yml: { language: "yaml", label: "YAML" },
  ini: { language: "ini", label: "INI" },
  cfg: { language: "ini", label: "Config" },
  conf: { language: "ini", label: "Config" },
  xml: { language: "xml", label: "XML" },
  svg: { language: "svg", label: "SVG" },
  html: { language: "html", label: "HTML" },
  htm: { language: "html", label: "HTML" },
  vue: { language: "vue", label: "Vue" },
  svelte: { language: "svelte", label: "Svelte" },
  css: { language: "css", label: "CSS" },
  scss: { language: "scss", label: "SCSS" },
  sass: { language: "sass", label: "Sass" },
  less: { language: "less", label: "Less" },
  sql: { language: "sql", label: "SQL" },
  lua: { language: "lua", label: "Lua" },
  zig: { language: "zig", label: "Zig" },
  proto: { language: "proto", label: "Protobuf" },
  graphql: { language: "graphql", label: "GraphQL" },
  gql: { language: "graphql", label: "GraphQL" },
  diff: { language: "diff", label: "Diff" },
  patch: { language: "diff", label: "Patch" },
  md: { language: "md", label: "Markdown" },
  markdown: { language: "md", label: "Markdown" },
  mdx: { language: "md", label: "MDX" },
  mdown: { language: "md", label: "Markdown" },
  mkd: { language: "md", label: "Markdown" },
  txt: { language: "txt", label: "Plain Text" },
};

export function detectFileKind(path) {
  const file = String(path || "");
  const base = file.split(/[\\/]/).pop() || "";
  const lowerBase = base.toLowerCase();
  if (lowerBase.startsWith(".env.")) {
    return { ext: "env", language: "ini", label: "Env", isMarkdown: false, isImage: false };
  }
  const exact = EXACT_FILES[lowerBase];
  if (exact) {
    return { ext: exact.language, ...exact, isMarkdown: false, isImage: false };
  }
  const dot = lowerBase.lastIndexOf(".");
  const ext = dot > 0 ? lowerBase.slice(dot + 1) : "";
  if (MARKDOWN_EXTS.has(ext)) {
    const known = EXT_LANGS[ext] ?? { language: "md", label: "Markdown" };
    return { ext, ...known, isMarkdown: true, isImage: false };
  }
  if (IMAGE_EXTS.has(ext)) {
    return { ext, language: ext, label: ext.toUpperCase(), isMarkdown: false, isImage: true };
  }
  const mapped = EXT_LANGS[ext];
  if (mapped) {
    return { ext, ...mapped, isMarkdown: false, isImage: false };
  }
  return {
    ext: ext || "txt",
    language: ext || "txt",
    label: ext ? ext.toUpperCase() : "Plain Text",
    isMarkdown: false,
    isImage: false,
  };
}

function normalizeHex(color, fallback = "#808080") {
  if (typeof color !== "string") return fallback;
  const trimmed = color.trim();
  if (/^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$/.test(trimmed)) return trimmed;
  if (/^#[0-9a-fA-F]{3}$/.test(trimmed)) {
    return `#${trimmed[1]}${trimmed[1]}${trimmed[2]}${trimmed[2]}${trimmed[3]}${trimmed[3]}`;
  }
  return fallback;
}

function withAlpha(color, aa) {
  const hex = normalizeHex(color);
  return `#${hex.slice(1, 7)}${aa}`;
}

export function isDarkHex(color) {
  const hex = normalizeHex(color, "#1e1e1e");
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return (0.299 * r + 0.587 * g + 0.114 * b) / 255 < 0.55;
}

export const CODE_THEMES = [
  { id: "auto", name: "Follow App", zhName: "跟随应用", light: null },
  {
    id: "vscode-dark-plus",
    name: "VS Code Dark+",
    zhName: "VS Code Dark+",
    light: false,
    colors: {
      background: "#1e1e1e",
      foreground: "#d4d4d4",
      gutter: "#1e1e1e",
      line_number: "#858585",
      active_line: "#2a2d2e",
      selection: "#264f78aa",
      caret: "#aeafad",
      border: "#2d2d30",
    },
    syntax: {
      keyword: "#569cd6",
      string: "#ce9178",
      "string.escape": "#d7ba7d",
      regexp: "#d16969",
      comment: "#6a9955",
      function: "#dcdcaa",
      macro: "#4ec9b0",
      type: "#4ec9b0",
      namespace: "#4ec9b0",
      number: "#b5cea8",
      boolean: "#569cd6",
      constant: "#4fc1ff",
      variable: "#9cdcfe",
      parameter: "#9cdcfe",
      property: "#9cdcfe",
      tag: "#569cd6",
      attribute: "#9cdcfe",
      operator: "#d4d4d4",
      punctuation: "#808080",
      heading: "#569cd6",
    },
  },
  {
    id: "one-dark-pro",
    name: "One Dark Pro",
    zhName: "One Dark Pro",
    light: false,
    colors: {
      background: "#282c34",
      foreground: "#abb2bf",
      gutter: "#282c34",
      line_number: "#495162",
      active_line: "#2c313c",
      selection: "#3e4451aa",
      caret: "#528bff",
      border: "#343942",
    },
    syntax: {
      keyword: "#c678dd",
      string: "#98c379",
      "string.escape": "#56b6c2",
      regexp: "#56b6c2",
      comment: "#7f848e",
      function: "#61afef",
      macro: "#c678dd",
      type: "#e5c07b",
      namespace: "#e5c07b",
      number: "#d19a66",
      boolean: "#d19a66",
      constant: "#d19a66",
      variable: "#e06c75",
      parameter: "#d19a66",
      property: "#e06c75",
      tag: "#e06c75",
      attribute: "#d19a66",
      operator: "#56b6c2",
      punctuation: "#abb2bf",
      heading: "#e06c75",
    },
  },
  {
    id: "github-dark",
    name: "GitHub Dark",
    zhName: "GitHub Dark",
    light: false,
    colors: {
      background: "#0d1117",
      foreground: "#e6edf3",
      gutter: "#0d1117",
      line_number: "#6e7681",
      active_line: "#161b22",
      selection: "#388bfd40",
      caret: "#58a6ff",
      border: "#30363d",
    },
    syntax: {
      keyword: "#ff7b72",
      string: "#a5d6ff",
      "string.escape": "#79c0ff",
      regexp: "#7ee787",
      comment: "#8b949e",
      function: "#d2a8ff",
      macro: "#d2a8ff",
      type: "#ffa657",
      namespace: "#ffa657",
      number: "#79c0ff",
      boolean: "#79c0ff",
      constant: "#79c0ff",
      variable: "#e6edf3",
      parameter: "#ffa657",
      property: "#79c0ff",
      tag: "#7ee787",
      attribute: "#79c0ff",
      operator: "#ff7b72",
      punctuation: "#8b949e",
      heading: "#79c0ff",
    },
  },
  {
    id: "github-light",
    name: "GitHub Light",
    zhName: "GitHub Light",
    light: true,
    colors: {
      background: "#ffffff",
      foreground: "#24292f",
      gutter: "#f6f8fa",
      line_number: "#8c959f",
      active_line: "#f6f8fa",
      selection: "#0969da2e",
      caret: "#0969da",
      border: "#d0d7de",
    },
    syntax: {
      keyword: "#cf222e",
      string: "#0a3069",
      "string.escape": "#0550ae",
      regexp: "#116329",
      comment: "#6e7781",
      function: "#8250df",
      macro: "#8250df",
      type: "#953800",
      namespace: "#953800",
      number: "#0550ae",
      boolean: "#0550ae",
      constant: "#0550ae",
      variable: "#24292f",
      parameter: "#953800",
      property: "#0550ae",
      tag: "#116329",
      attribute: "#0550ae",
      operator: "#cf222e",
      punctuation: "#57606a",
      heading: "#0550ae",
    },
  },
  {
    id: "dracula",
    name: "Dracula",
    zhName: "Dracula",
    light: false,
    colors: {
      background: "#282a36",
      foreground: "#f8f8f2",
      gutter: "#282a36",
      line_number: "#6272a4",
      active_line: "#44475a66",
      selection: "#44475acc",
      caret: "#f8f8f2",
      border: "#3b3e4f",
    },
    syntax: {
      keyword: "#ff79c6",
      string: "#f1fa8c",
      "string.escape": "#ff79c6",
      regexp: "#ff5555",
      comment: "#6272a4",
      function: "#50fa7b",
      macro: "#50fa7b",
      type: "#8be9fd",
      namespace: "#8be9fd",
      number: "#bd93f9",
      boolean: "#bd93f9",
      constant: "#bd93f9",
      variable: "#f8f8f2",
      parameter: "#ffb86c",
      property: "#50fa7b",
      tag: "#ff79c6",
      attribute: "#50fa7b",
      operator: "#ff79c6",
      punctuation: "#f8f8f2",
      heading: "#bd93f9",
    },
  },
  {
    id: "tokyo-night",
    name: "Tokyo Night",
    zhName: "Tokyo Night",
    light: false,
    colors: {
      background: "#1a1b26",
      foreground: "#c0caf5",
      gutter: "#1a1b26",
      line_number: "#3b4261",
      active_line: "#292e42",
      selection: "#33467cbb",
      caret: "#c0caf5",
      border: "#292e42",
    },
    syntax: {
      keyword: "#9d7cd8",
      string: "#9ece6a",
      "string.escape": "#89ddff",
      regexp: "#b4f9f8",
      comment: "#565f89",
      function: "#7aa2f7",
      macro: "#7dcfff",
      type: "#2ac3de",
      namespace: "#7dcfff",
      number: "#ff9e64",
      boolean: "#ff9e64",
      constant: "#ff9e64",
      variable: "#c0caf5",
      parameter: "#e0af68",
      property: "#73daca",
      tag: "#f7768e",
      attribute: "#bb9af7",
      operator: "#89ddff",
      punctuation: "#9abdf5",
      heading: "#7aa2f7",
    },
  },
  {
    id: "catppuccin-mocha",
    name: "Catppuccin Mocha",
    zhName: "Catppuccin Mocha",
    light: false,
    colors: {
      background: "#1e1e2e",
      foreground: "#cdd6f4",
      gutter: "#181825",
      line_number: "#6c7086",
      active_line: "#31324499",
      selection: "#45475abb",
      caret: "#f5e0dc",
      border: "#313244",
    },
    syntax: {
      keyword: "#cba6f7",
      string: "#a6e3a1",
      "string.escape": "#f5c2e7",
      regexp: "#f5c2e7",
      comment: "#6c7086",
      function: "#89b4fa",
      macro: "#f38ba8",
      type: "#f9e2af",
      namespace: "#f9e2af",
      number: "#fab387",
      boolean: "#fab387",
      constant: "#fab387",
      variable: "#cdd6f4",
      parameter: "#eba0ac",
      property: "#94e2d5",
      tag: "#89b4fa",
      attribute: "#f9e2af",
      operator: "#89dceb",
      punctuation: "#9399b2",
      heading: "#89b4fa",
    },
  },
  {
    id: "solarized-light",
    name: "Solarized Light",
    zhName: "Solarized Light",
    light: true,
    colors: {
      background: "#fdf6e3",
      foreground: "#586e75",
      gutter: "#eee8d5",
      line_number: "#93a1a1",
      active_line: "#eee8d5",
      selection: "#268bd230",
      caret: "#657b83",
      border: "#e2dcc7",
    },
    syntax: {
      keyword: "#859900",
      string: "#2aa198",
      "string.escape": "#dc322f",
      regexp: "#dc322f",
      comment: "#93a1a1",
      function: "#268bd2",
      macro: "#cb4b16",
      type: "#b58900",
      namespace: "#b58900",
      number: "#d33682",
      boolean: "#d33682",
      constant: "#cb4b16",
      variable: "#586e75",
      parameter: "#6c71c4",
      property: "#268bd2",
      tag: "#268bd2",
      attribute: "#93a1a1",
      operator: "#657b83",
      punctuation: "#93a1a1",
      heading: "#268bd2",
    },
  },
];

const CODE_THEME_BY_ID = Object.fromEntries(CODE_THEMES.map(item => [item.id, item]));

export function resolveCodeTheme(id, appTheme) {
  const t = appTheme ?? {};
  const preset = CODE_THEME_BY_ID[id];
  if (preset && preset.id !== "auto") {
    return {
      ...preset,
      swatch: preset.syntax.keyword,
      bgSwatch: preset.colors.background,
    };
  }
  const background = normalizeHex(t.surface || t.background, "#181a1f");
  const foreground = normalizeHex(t.foreground, isDarkHex(background) ? "#e5e7eb" : "#1f2328");
  const gutter = normalizeHex(t.background || t.surface, background);
  const lineNumber = normalizeHex(t.dim || t.muted, "#7f848e");
  const activeLine = normalizeHex(t.hover || t.raised, withAlpha(foreground, "12"));
  const selection = normalizeHex(t.selection, withAlpha(t.accent || "#3b82f6", "44"));
  const caret = normalizeHex(t.accent, foreground);
  const border = normalizeHex(t.border, "#2c313c");
  const syntax = {
    keyword: normalizeHex(t.ansi5, "#c678dd"),
    string: normalizeHex(t.ansi2, "#98c379"),
    "string.escape": normalizeHex(t.ansi6, "#56b6c2"),
    regexp: normalizeHex(t.ansi6, "#56b6c2"),
    comment: normalizeHex(t.dim, "#7f848e"),
    function: normalizeHex(t.ansi4, "#61afef"),
    macro: normalizeHex(t.ansi5, "#c678dd"),
    type: normalizeHex(t.ansi3, "#e5c07b"),
    namespace: normalizeHex(t.ansi3, "#e5c07b"),
    number: normalizeHex(t.ansi1, "#d19a66"),
    boolean: normalizeHex(t.ansi1, "#d19a66"),
    constant: normalizeHex(t.ansi6, "#56b6c2"),
    variable: foreground,
    parameter: normalizeHex(t.ansi1, "#d19a66"),
    property: normalizeHex(t.ansi4, "#61afef"),
    tag: normalizeHex(t.ansi5, "#c678dd"),
    attribute: normalizeHex(t.ansi3, "#e5c07b"),
    operator: normalizeHex(t.ansi6, "#56b6c2"),
    punctuation: lineNumber,
    heading: normalizeHex(t.ansi4, "#61afef"),
  };
  return {
    id: "auto",
    name: "Follow App",
    zhName: "跟随应用",
    light: !isDarkHex(background),
    swatch: syntax.keyword,
    bgSwatch: background,
    colors: {
      background,
      foreground,
      gutter,
      line_number: lineNumber,
      active_line: activeLine,
      selection,
      caret,
      border,
    },
    syntax,
  };
}

export const MD_THEMES = [
  { id: "auto", name: "Follow App", zhName: "跟随应用" },
  {
    id: "github-light",
    name: "GitHub Light",
    zhName: "GitHub 亮色",
    light: true,
    background: "#ffffff",
    surface: "#f6f8fa",
    border: "#d0d7de",
    editorThemeId: "github-light",
    textView: {
      dark: false,
      foreground: "#1f2328",
      muted: "#656d76",
      link: "#0969da",
      selection: "#0969da2e",
      code_background: "#f6f8fa",
      inline_code_foreground: "#cf222e",
      inline_code_background: "#eff1f3",
      border: "#d0d7de",
      heading: "#1f2328",
    },
  },
  {
    id: "github-dark",
    name: "GitHub Dark",
    zhName: "GitHub 暗色",
    light: false,
    background: "#0d1117",
    surface: "#161b22",
    border: "#30363d",
    editorThemeId: "github-dark",
    textView: {
      dark: true,
      foreground: "#e6edf3",
      muted: "#8b949e",
      link: "#58a6ff",
      selection: "#388bfd40",
      code_background: "#161b22",
      inline_code_foreground: "#ffa657",
      inline_code_background: "#262c36",
      border: "#30363d",
      heading: "#f0f6fc",
    },
  },
  {
    id: "notion-warm",
    name: "Notion Paper",
    zhName: "Notion 暖白",
    light: true,
    background: "#fbfaf6",
    surface: "#f3f1eb",
    border: "#e6e2d8",
    editorThemeId: "solarized-light",
    textView: {
      dark: false,
      foreground: "#37352f",
      muted: "#787774",
      link: "#0b6e99",
      selection: "#2eaadc33",
      code_background: "#f3f1eb",
      inline_code_foreground: "#eb5757",
      inline_code_background: "#ece9df",
      border: "#e6e2d8",
      heading: "#2f2d28",
    },
  },
  {
    id: "one-dark",
    name: "One Dark",
    zhName: "One Dark",
    light: false,
    background: "#282c34",
    surface: "#21252b",
    border: "#3e4451",
    editorThemeId: "one-dark-pro",
    textView: {
      dark: true,
      foreground: "#abb2bf",
      muted: "#7f848e",
      link: "#61afef",
      selection: "#3e4451aa",
      code_background: "#21252b",
      inline_code_foreground: "#e06c75",
      inline_code_background: "#2c313c",
      border: "#3e4451",
      heading: "#e5c07b",
    },
  },
  {
    id: "tokyo-night",
    name: "Tokyo Night",
    zhName: "Tokyo Night",
    light: false,
    background: "#1a1b26",
    surface: "#16161e",
    border: "#292e42",
    editorThemeId: "tokyo-night",
    textView: {
      dark: true,
      foreground: "#c0caf5",
      muted: "#7aa2f7",
      link: "#7dcfff",
      selection: "#33467cbb",
      code_background: "#16161e",
      inline_code_foreground: "#bb9af7",
      inline_code_background: "#24283b",
      border: "#292e42",
      heading: "#7dcfff",
    },
  },
  {
    id: "catppuccin-mocha",
    name: "Catppuccin Mocha",
    zhName: "Catppuccin Mocha",
    light: false,
    background: "#1e1e2e",
    surface: "#181825",
    border: "#313244",
    editorThemeId: "catppuccin-mocha",
    textView: {
      dark: true,
      foreground: "#cdd6f4",
      muted: "#a6adc8",
      link: "#89b4fa",
      selection: "#45475abb",
      code_background: "#181825",
      inline_code_foreground: "#f5c2e7",
      inline_code_background: "#313244",
      border: "#313244",
      heading: "#cba6f7",
    },
  },
  {
    id: "solarized-paper",
    name: "Solarized Paper",
    zhName: "Solarized 护眼",
    light: true,
    background: "#fdf6e3",
    surface: "#eee8d5",
    border: "#ded7c0",
    editorThemeId: "solarized-light",
    textView: {
      dark: false,
      foreground: "#586e75",
      muted: "#839496",
      link: "#268bd2",
      selection: "#268bd230",
      code_background: "#eee8d5",
      inline_code_foreground: "#cb4b16",
      inline_code_background: "#e9e2ce",
      border: "#ded7c0",
      heading: "#268bd2",
    },
  },
];

const MD_THEME_BY_ID = Object.fromEntries(MD_THEMES.map(item => [item.id, item]));

export function resolveMdTheme(id, appTheme) {
  const t = appTheme ?? {};
  const preset = MD_THEME_BY_ID[id];
  if (preset && preset.id !== "auto") {
    const codeTheme = resolveCodeTheme(preset.editorThemeId, appTheme);
    return {
      ...preset,
      swatch: preset.textView.link,
      bgSwatch: preset.background,
      textView: {
        ...preset.textView,
        syntax: codeTheme.syntax,
      },
    };
  }
  const background = normalizeHex(t.background || t.surface, "#14161a");
  const surface = normalizeHex(t.surface || t.raised || background, "#1b1e24");
  const dark = isDarkHex(background);
  const foreground = normalizeHex(t.foreground, dark ? "#e5e7eb" : "#1f2328");
  const muted = normalizeHex(t.muted || t.dim, dark ? "#9ca3af" : "#6b7280");
  const link = normalizeHex(t.accent || t.ansi4, "#3b82f6");
  const border = normalizeHex(t.border, dark ? "#2e323b" : "#d0d7de");
  const codeBackground = normalizeHex(t.raised || t.surface, surface);
  const inlineBg = normalizeHex(t.hover || t.raised || codeBackground, codeBackground);
  const inlineFg = normalizeHex(t.ansi5 || t.accent || foreground, foreground);
  const selection = normalizeHex(t.selection, withAlpha(link, "3d"));
  const codeTheme = resolveCodeTheme("auto", appTheme);
  return {
    id: "auto",
    name: "Follow App",
    zhName: "跟随应用",
    light: !dark,
    background,
    surface,
    border,
    swatch: link,
    bgSwatch: background,
    editorThemeId: "auto",
    textView: {
      dark,
      foreground,
      muted,
      link,
      selection,
      code_background: codeBackground,
      inline_code_foreground: inlineFg,
      inline_code_background: inlineBg,
      border,
      heading: foreground,
      syntax: codeTheme.syntax,
    },
  };
}

const ALERT_LABELS = {
  NOTE: "ⓘ Note",
  TIP: "💡 Tip",
  IMPORTANT: "❗ Important",
  WARNING: "⚠ Warning",
  CAUTION: "🛑 Caution",
};

export function prepareMarkdown(raw) {
  const source = String(raw ?? "");
  if (!source.includes("[!")) return source;
  const lines = source.split("\n");
  let fenceChar = "";
  let fenceLen = 0;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fence = line.match(/^\s*(`{3,}|~{3,})/);
    if (fence) {
      const marker = fence[1];
      if (!fenceChar) {
        fenceChar = marker[0];
        fenceLen = marker.length;
      } else if (marker[0] === fenceChar && marker.length >= fenceLen) {
        fenceChar = "";
        fenceLen = 0;
      }
      continue;
    }
    if (fenceChar) continue;
    const alert = line.match(/^(\s*>\s*)\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]\s*(.*)$/i);
    if (alert) {
      const kind = alert[2].toUpperCase();
      const label = ALERT_LABELS[kind] ?? kind;
      const tail = alert[3] ? ` ${alert[3]}` : "";
      lines[i] = `${alert[1]}**${label}**${tail}`;
    }
  }
  return lines.join("\n");
}

export function resolveRelativeFileLink(currentFile, href) {
  const target = String(href ?? "").trim();
  if (!target || target.startsWith("#")) return null;
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(target) || target.startsWith("//")) return null;
  const clean = target.split(/[?#]/)[0];
  if (!clean || clean.startsWith("/")) return null;
  const baseParts = String(currentFile ?? "").split("/").filter(Boolean);
  baseParts.pop();
  const segments = [...baseParts];
  for (const part of clean.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (segments.length === 0) return null;
      segments.pop();
    } else {
      segments.push(part);
    }
  }
  return segments.length ? segments.join("/") : null;
}

export function resolveRelativeAssetPath(currentFile, rawHref) {
  let target = String(rawHref ?? "").trim();
  if (target.startsWith("<") && target.endsWith(">")) target = target.slice(1, -1).trim();
  if (!target || target.startsWith("#") || target.startsWith("//")) return null;
  if (/^[a-zA-Z][a-zA-Z0-9+.-]{1,}:/.test(target)) return null;
  const clean = target.split(/[?#]/)[0].trim();
  if (!clean) return null;
  let decoded = clean;
  try { decoded = decodeURIComponent(clean); } catch {}
  const normalized = decoded.replace(/\\/g, "/");
  if (normalized.startsWith("/")) {
    const rel = normalized.replace(/^\/+/, "");
    return rel || null;
  }
  const baseParts = String(currentFile ?? "").split("/").filter(Boolean);
  baseParts.pop();
  const segments = [...baseParts];
  for (const part of normalized.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (segments.length === 0) return null;
      segments.pop();
    } else {
      segments.push(part);
    }
  }
  return segments.length ? segments.join("/") : null;
}
