import type { BuiltInWorkerRole, Effort, Profile, RoleAccessMode } from "./types";

export const BUILT_IN_ROLE_IDS = [
  "coder",
  "tester",
  "writer",
  "reviewer",
  "researcher",
] as const satisfies readonly BuiltInWorkerRole[];

export type RoleVisualKind = "coordinator" | BuiltInWorkerRole | "custom";

export interface RoleMeta {
  id: string;
  builtIn: boolean;
  visualKind: RoleVisualKind;
  labelKey: string;
  fallbackLabel: string;
  access: RoleAccessMode;
  accessKey: `role.access.${RoleAccessMode}`;
  badgeClass: string;
  defaultAgents: string[];
  defaultEffort: Effort;
  tags: string[];
  allowedPaths: string[];
}

interface BuiltInMetaSpec {
  id: BuiltInWorkerRole;
  labelKey: string;
  fallbackLabel: string;
  access: RoleAccessMode;
  defaultEffort: Effort;
  defaultAgents: string[];
  tags: string[];
  allowedPaths: string[];
}

const BUILTIN_SPECS: Record<BuiltInWorkerRole, BuiltInMetaSpec> = {
  coder: {
    id: "coder",
    labelKey: "role.coder",
    fallbackLabel: "程序员",
    access: "writable",
    defaultEffort: "high",
    defaultAgents: ["antigravity/gemini-4-hydrogen", "codex/gpt-6.1-sol", "claude/zcode/GLM-5.3[1m]"],
    tags: ["backend", "refactor"],
    allowedPaths: [],
  },
  tester: {
    id: "tester",
    labelKey: "role.tester",
    fallbackLabel: "测试员",
    access: "scoped_write",
    defaultEffort: "medium",
    defaultAgents: ["dsh", "antigravity/gemini-4-hydrogen", "cursor/grok-4.7", "claude/zcode/GLM-5.3-Flash[1m]"],
    tags: ["tests"],
    allowedPaths: ["test/**", "tests/**", "__tests__/**", "**/*.test.*", "**/*.spec.*", "**/fixtures/**", "**/testdata/**"],
  },
  writer: {
    id: "writer",
    labelKey: "role.writer",
    fallbackLabel: "技术作家",
    access: "scoped_write",
    defaultEffort: "medium",
    defaultAgents: ["antigravity/gemini-4-carbon", "antigravity/gemini-4-argon", "claude"],
    tags: ["docs"],
    allowedPaths: ["docs/**", "skills/**", "**/*.md", "**/*.mdx", "**/*.rst", "**/*.txt", "**/*.svg", "**/*.mmd", "**/*.puml"],
  },
  reviewer: {
    id: "reviewer",
    labelKey: "role.reviewer",
    fallbackLabel: "审查员",
    access: "readonly",
    defaultEffort: "high",
    defaultAgents: ["codex/gpt-6.1-sol", "antigravity/gemini-4-hydrogen"],
    tags: ["review"],
    allowedPaths: [],
  },
  researcher: {
    id: "researcher",
    labelKey: "role.researcher",
    fallbackLabel: "调研员",
    access: "readonly",
    defaultEffort: "high",
    defaultAgents: ["dsh", "claude/zcode/GLM-5.3-Flash[1m]", "antigravity/gemini-4-carbon"],
    tags: ["research", "data"],
    allowedPaths: [],
  },
};

export function isBuiltInRole(role: string | undefined): role is BuiltInWorkerRole {
  return Boolean(role && role in BUILTIN_SPECS);
}

export function resolveRoleMeta(
  role: string | undefined,
  opts?: { readOnlyDir?: string; profile?: Profile | null },
): RoleMeta {
  const trimmed = role?.trim();
  const normalized =
    trimmed === "reviewer" || (!trimmed && opts?.readOnlyDir)
      ? "reviewer"
      : !trimmed
        ? "coder"
        : trimmed;

  const profileCfg = opts?.profile?.roles?.[normalized];

  if (isBuiltInRole(normalized)) {
    const spec = BUILTIN_SPECS[normalized];
    const access = profileCfg?.access ?? spec.access;
    return {
      id: normalized,
      builtIn: true,
      visualKind: normalized,
      labelKey: spec.labelKey,
      fallbackLabel: spec.fallbackLabel,
      access,
      accessKey: `role.access.${access}`,
      badgeClass: `role-badge--${normalized}`,
      defaultAgents: profileCfg?.agents?.length ? profileCfg.agents : spec.defaultAgents,
      defaultEffort: profileCfg?.effort ?? spec.defaultEffort,
      tags: profileCfg?.tags?.length ? profileCfg.tags : spec.tags,
      allowedPaths: profileCfg?.allowedPaths?.length ? profileCfg.allowedPaths : spec.allowedPaths,
    };
  }

  const access: RoleAccessMode = profileCfg?.access ?? (opts?.readOnlyDir ? "readonly" : "writable");
  return {
    id: normalized,
    builtIn: false,
    visualKind: "custom",
    labelKey: "role.custom",
    fallbackLabel: normalized,
    access,
    accessKey: `role.access.${access}`,
    badgeClass: "role-badge--custom",
    defaultAgents: profileCfg?.agents ?? [],
    defaultEffort: profileCfg?.effort ?? "medium",
    tags: profileCfg?.tags ?? [],
    allowedPaths: profileCfg?.allowedPaths ?? [],
  };
}

export function roleDisplayName(
  meta: RoleMeta,
  t: (key: string, vars?: Record<string, string | number>) => string,
): string {
  return meta.builtIn ? t(meta.labelKey) : meta.fallbackLabel;
}

export interface RoleSelectOption {
  value: string;
  labelKey: string;
  fallbackLabel: string;
  builtIn: boolean;
}

export function buildRoleSelectOptions(profile?: Profile | null, currentRole?: string): RoleSelectOption[] {
  const options: RoleSelectOption[] = [
    { value: "", labelKey: "role.defaultCoder", fallbackLabel: "默认（程序员 coder）", builtIn: true },
  ];
  const seen = new Set<string>([""]);
  for (const id of BUILT_IN_ROLE_IDS) {
    seen.add(id);
    options.push({
      value: id,
      labelKey: BUILTIN_SPECS[id].labelKey,
      fallbackLabel: BUILTIN_SPECS[id].fallbackLabel,
      builtIn: true,
    });
  }
  for (const customId of Object.keys(profile?.roles ?? {})) {
    const trimmed = customId.trim();
    if (!trimmed || seen.has(trimmed)) continue;
    seen.add(trimmed);
    options.push({ value: trimmed, labelKey: "role.custom", fallbackLabel: trimmed, builtIn: false });
  }
  const cur = currentRole?.trim();
  if (cur && !seen.has(cur)) {
    options.push({ value: cur, labelKey: "role.custom", fallbackLabel: cur, builtIn: false });
  }
  return options;
}
