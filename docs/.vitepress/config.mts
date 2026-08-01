import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vitepress";

import spec from "../cli/commands.json";

interface Cmd {
  full_cmd: string[];
  subcommands: Record<string, Cmd>;
  hide?: boolean;
}

function getCommands(cmd: Cmd): string[][] {
  const commands: string[][] = [];
  for (const sub of Object.values(cmd.subcommands)) {
    if (sub.hide) continue;
    commands.push(sub.full_cmd);
    commands.push(...getCommands(sub));
  }
  return commands;
}

const commands = getCommands(spec.cmd);
const configDir = dirname(fileURLToPath(import.meta.url));
const cargoToml = readFileSync(resolve(configDir, "../../Cargo.toml"), "utf8");
const versionMatch = cargoToml.match(
  /^\[package\][\s\S]*?^\s*version\s*=\s*"([^"]+)"/m,
);
const latestVersion = versionMatch?.[1] ?? "0.0.0";

export default defineConfig({
  title: "tak",
  description: "Deterministic CLI benchmarking with instruction counts",
  cleanUrls: true,
  lastUpdated: true,

  head: [
    ["link", { rel: "icon", href: "/favicon.ico", sizes: "48x48" }],
    ["link", { rel: "icon", href: "/favicon.svg", type: "image/svg+xml" }],
    ["link", { rel: "apple-touch-icon", href: "/apple-touch-icon.png" }],
    ["meta", { name: "theme-color", content: "#f97316" }],
    ["meta", { property: "og:type", content: "website" }],
    ["meta", { property: "og:title", content: "tak" }],
    [
      "meta",
      {
        property: "og:description",
        content: "Deterministic CLI benchmarking with instruction counts",
      },
    ],
    [
      "meta",
      {
        property: "og:image",
        content: "https://tak.jdx.dev/android-chrome-512x512.png",
      },
    ],
    ["meta", { name: "twitter:card", content: "summary" }],
  ],

  themeConfig: {
    logo: {
      light: "/logo-light.svg",
      dark: "/logo-dark.svg",
      alt: "tak logo",
    },

    nav: [
      { text: "Methodology", link: "/guide/methodology" },
      { text: "Getting started", link: "/guide/getting-started" },
      { text: "CLI reference", link: "/cli/" },
      {
        text: `v${latestVersion}`,
        link: "https://github.com/jdx/tak/releases",
      },
    ],

    sidebar: [
      {
        text: "Guide",
        items: [
          { text: "Methodology", link: "/guide/methodology" },
          { text: "Getting started", link: "/guide/getting-started" },
          { text: "Adopt tak in a project", link: "/guide/adopting" },
          { text: "Benchmark configuration", link: "/guide/configuration" },
          { text: "CI and git notes", link: "/guide/ci" },
        ],
      },
      {
        text: "CLI reference",
        link: "/cli/",
        collapsed: true,
        items: commands.map((cmd) => ({
          text: cmd.join(" "),
          link: `/cli/${cmd.join("/")}`,
        })),
      },
    ],

    outline: { level: [2, 3] },
    socialLinks: [{ icon: "github", link: "https://github.com/jdx/tak" }],
    editLink: {
      pattern: "https://github.com/jdx/tak/edit/main/docs/:path",
      text: "Edit this page on GitHub",
    },
    search: { provider: "local" },
    footer: false,
  },
});
