import { describe, expect, test } from "bun:test";
import { checkExample, checkMarkdown, commandPage, type Reference } from "./docs";

const ref: Reference = {
  help: "Usage:\n  herdr2 status\n  herdr2 ls\n  herdr2 do <command> [args]\n  herdr2 commands\n  herdr2 --skill\n  herdr2 --version\n",
  commands: [{ name: "pane.split", about: "Split a pane.", args: [{ name: "direction", required: true }] }],
};

describe("CLI examples", () => {
  test("validates commands and named arguments without executing examples", () => {
    for (const source of ["herdr2", "herdr2 ls", "herdr2 --help", "herdr2 --skill", "herdr2 do pane.split --direction right", `herdr2 do pane.split '{"direction":"right"}'`]) {
      expect(checkExample(source, ref)).toEqual([]);
    }
    expect(checkExample("herdr2 wat", ref)).toEqual(["unknown command or flag: wat"]);
    expect(checkExample("herdr2 ls && herdr2 wat", ref)).toEqual(["unknown command or flag: wat"]);
    expect(checkExample("herdr2 --wat", ref)).toHaveLength(1);
    expect(checkExample("herdr2 ls --wat", ref)).toHaveLength(1);
    expect(checkExample("herdr2 do pane.wat", ref)).toHaveLength(1);
    expect(checkExample("herdr2 do pane.split --wat right", ref)).toHaveLength(1);
    expect(checkExample("herdr2 do pane.split --direction=right", ref)).toContain("unknown flag for do: --direction=right");
    expect(checkExample("herdr2 do pane.split --direction='right'", ref)).toContain("unknown flag for do: --direction=right");
    expect(checkExample('herdr2 ls "--wat"', ref)).toEqual(["unknown flag for ls: --wat"]);
    expect(checkExample('herdr2 do pane.split "--wat" right', ref)).toEqual(["unknown flag for do: --wat"]);
    expect(checkExample('herdr2 do pane.split "--direction" "right"', ref)).toEqual([]);
    expect(checkExample("herdr2 do pane.split --direction", ref)).toEqual(["missing value for --direction"]);
  });

  test("ignores flags inside quotes and comments, and other programs", () => {
    expect(checkExample(`herdr2 do pane.split --direction '--not-a-flag' # --comment`, ref)).toEqual([]);
    expect(checkExample("curl --not-our-flag https://example.org", ref)).toEqual([]);
    expect(checkExample("herdr2 ls | other --not-our-flag", ref)).toEqual([]);
    expect(checkExample("herdr2 do pane.split --direction herdr2", ref)).toEqual([]);
  });

  test("reports inline and fenced examples with file and line", () => {
    expect(checkMarkdown("README.md", "Text `herdr2 wat`.\n\n```sh\nherdr2 ls --wat\n```", ref)).toEqual([
      "README.md:1: unknown command or flag: wat",
      "README.md:4: unknown flag for ls: --wat",
    ]);
    expect(checkMarkdown("docs/example.md", "~~~sh\nherdr2 do pane.split \\\n  --wat right\n~~~", ref)).toEqual(["docs/example.md:2: unknown flag for do: --wat"]);
    expect(checkMarkdown("README.md", "Plain prose herdr2 wat is not code.", ref)).toEqual([]);
  });
});

test("command reference comes from the catalog", () => {
  const page = commandPage(ref.commands);
  expect(page).toContain("## `pane.split`");
  expect(page).toContain("| `direction` | yes |");
  expect(page).toContain("Split a pane.");
  expect(checkMarkdown("docs/commands.md", page, ref)).toEqual([]);
});

