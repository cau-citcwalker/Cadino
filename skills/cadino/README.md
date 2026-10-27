# Cadino skill

Teaches an AI agent the Cadino MCP workflow for furniture: brief, model as closed solids, verify,
clean up stray geometry, drawings, cut list and export to Rhino. It needs Cadino's MCP server
(`cadcraft-cli mcp`, set up in Cadino under Options ▸ AI).

| File | For |
|---|---|
| `SKILL.md`, `references/` | Claude Code and Claude Desktop (skill format) |
| `AGENTS.md` | Codex and other agents that read AGENTS.md |

## Install

- **Claude Code:** copy this folder to `~/.claude/skills/cadino/` (every project) or to
  `.claude/skills/cadino/` in a project. Claude loads it when a request is about Cadino or
  furniture modelling.
- **Claude Desktop:** zip this folder (the zip holds `cadino/SKILL.md`), then Settings ▸
  Capabilities ▸ Skills ▸ Upload skill. Skills need code execution turned on in the same settings.
- **Codex:** paste `AGENTS.md` into `~/.codex/AGENTS.md` (every project) or into the `AGENTS.md`
  at a project's root. A Codex version that supports skills can load this folder from
  `~/.codex/skills/cadino/` instead.
