# CADCraft and AI agents (MCP)

CADCraft speaks the [Model Context Protocol](https://modelcontextprotocol.io) over stdio.

## Start it

```sh
# Headless: an in-process drawing session, no window.
cadcraft-cli mcp

# Bridged to the running app (watch the agent draw):
cadcraft --control 7979 &
cadcraft-cli mcp --connect 127.0.0.1:7979
```

An MCP client usually starts its servers when it starts, often before the app is open. With
`--lazy` the bridge starts anyway and reaches the app at the first tool call; until the app answers,
each call fails with a message saying how to open it. `--launch` also opens CADCraft (macOS) with
that control port at a call that finds nothing, and waits for it:

```json
{ "mcpServers": { "cadcraft": { "command": "cadcraft-cli", "args": ["mcp", "--connect", "127.0.0.1:7979", "--launch"] } } }
```

`--connect` needs the app's `HOST:PORT`. Without one (or with an unknown argument) `cadcraft-cli mcp`
exits with an error instead of falling back to a headless session, so a client meant for the app
never edits a separate drawing by mistake. If the app can't be reached, startup fails too.

In the app, Options ▸ AI can start and stop the control server (port 7979 by default) and shows
ready-to-copy configurations for Claude Desktop, Claude Code, Codex CLI and Cursor that use the
`cadcraft-cli` next to the app with `--connect 127.0.0.1:PORT --lazy`. The app's own AI Assistant
palette (Tools ▸ AI Assistant) uses the same tools.

Example client configuration (Claude Code / Claude Desktop):

```json
{ "mcpServers": { "cadcraft": { "command": "cadcraft-cli", "args": ["mcp"] } } }
```

## Tools

| Tool | What it does |
|---|---|
| `command_line {text}` | Type at the command line exactly like a person: `line 0,0 @10,0 @0,5 c`, `circle 5,2 1`, `offset 0.5`, `zoom e`. Spaces act as Enter; empty text is Enter. Returns the new prompt, keywords and output. |
| `script {text}` | A multi-line script (like `.scr` files). |
| `execute {command, params}` | Any command with JSON parameters, never a dialog. `list_commands` shows ids and parameter docs. |
| `list_commands {filter?}` | The command catalog; `filter` matches part of a command's id, label or an alias. |
| `inspect_drawing {entities?, limit?}` | Counts, extents, layers, styles, blocks, layouts, selection, undo history and entities. |
| `query_entities {type?, layer?, window?, crossing?, limit?, offset?}` | Filtered entities with handles and geometry. `window` `[[x0,y0],[x1,y1]]` finds objects whose geometry is inside it or crosses it (`crossing: false`: entirely inside). `count` is every match, `returned` the page (`offset`, `limit`, default 500). |
| `render {width?, height?, fit?, window?, view?}` | A PNG image of the drawing, or of just the area `window` `[[x0,y0],[x1,y1]]` (one sheet or room of a large drawing; 1600 px wide with the window's shape unless sized). `view` (`plan`/`top`, `front`, `back`, `left`, `right`, `iso` = south-west, `iso_se`, `iso_ne`, `iso_nw`) looks at model space from that side, shaded with edges on white, without changing the drawing's view. |
| `find_text {contains?, layer?, window?, limit?, offset?}` | The readable texts — TEXT, MTEXT without formatting codes, block attributes, multileaders — with text, insertion point, height, rotation, style, layer and handle; filtered by content (any case), layer and a window around the insertion points. Read-only (the `texts` command). |
| `new_drawing {metric?}`, `open {path}`, `save {path?}`, `cancel` | Files and Escape. `save` to a `.svg`, `.png` or `.pdf` path exports: the drawing keeps its name and unsaved changes. |
| `screenshot`, `ui_inspect`, `ui_click {x, y}` | Only when connected to the app. |

Resources: `cadcraft://drawing` (inspect JSON) and `cadcraft://commands` (catalog).

Every tool carries MCP tool annotations (2025-06-18): a `title`, `openWorldHint: false`, and
`readOnlyHint: true` on `list_commands`, `inspect_drawing`, `query_entities`, `render`, `find_text`
and `ui_inspect`, so a client may run those without asking. The others are marked
`destructiveHint: true` (with `idempotentHint` true for `save`, `cancel` and `screenshot`):
`screenshot` always writes a PNG, so it isn't read-only. `render` takes no `path` and writes no
file (a `path` sent anyway is dropped); `save` to a `.png` path writes a picture, and the control
method `ui.render` still takes `path`. The in-app assistant sends Claude and GPT only each tool's
name, description and schema, never the annotations.

In tool results a 3D solid's B-rep text and mesh are left out; the mesh shows as
`<mesh, N triangles>`, and an open one, a surface rather than a closed solid, as
`<mesh, N triangles, open: a surface, not a solid>`. The working instructions tell the agent to
look for it: lines extruded make loose surfaces, which Rhino shows as separate pieces.

`skills/cadino/` is a skill for agents driving Cadino over MCP (Claude Code and Claude Desktop:
`SKILL.md` and `references/recipes.md`; Codex: `AGENTS.md`; install steps in its `README.md`):
the furniture workflow from brief to closed solids, checks, clean-up, drawings, cut list and
export to Rhino. The portable zip ships it next to the app.

Arguments are checked against each tool's input schema. An unknown tool, `arguments` that aren't an
object, a missing required argument or one of the wrong type is a JSON-RPC error `-32602` (invalid
params), so `command_line {}` never presses Enter by accident. A tool that runs and fails (a bad
point, an unknown command) returns a result with `isError: true` and the message.

## A typical session

1. `command_line {"text": "rectang 0,0 100,60"}`
2. `execute {"command": "circle", "params": {"center": [10, 10], "radius": 4}}`
3. `command_line {"text": "dimlinear 0,0 100,0 50,-8"}`
4. `inspect_drawing {}` — verify counts and geometry.
5. `render {}` — look at it.

The end-to-end test in `crates/mcp/src/tests.rs` runs a session like this and checks the result
only through MCP.
