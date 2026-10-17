# AI in Cadino

> **Last updated:** 2026-10-17 · **Status:** landed (assistant, model settings, MCP switch, hardening and polish) · **Describes:** `main`

The user guide for Cadino's AI features: the built-in AI Assistant, the models it can use, the
switch that lets external AI apps drive Cadino over MCP, what leaves your computer, and what to do
when something goes wrong. The tools themselves are listed in [mcp.md](mcp.md).

## What it does

Describe a piece of furniture or a room in plain words, in any language, and the assistant drafts
and models it in the open drawing: vibe CAD, with AutoCAD's 2D and Rhino-style 3D underneath.

| You ask | The assistant |
|---|---|
| "A 1400 × 700 mm desk top with R30 corners and a Ø60 cable hole" | Draws it in plan on its own layer and dimensions it |
| "A 3600 × 3000 mm bedroom with 100 mm walls and a 900 mm door" | Drafts the walls, the opening and the dimensions |
| "A 4-door wardrobe 1200 × 600 × 2000 mm with 18 mm panels" | Models each board as a named part (PANEL), then makes a cut list |
| "A turned table leg", "a lofted lounge chair seat" | Draws profiles on construction planes, then revolves or lofts them |
| "Front, top and right drawings of the model" | Lays out VIEWBASE views with dimensions on a layout |
| "Check this drawing" | Reads units, layers, extents and labels and reports them |

Everything it does is an ordinary Cadino command. It shows in the drawing at once, and Undo
takes it back. Its instructions tell it to turn a loose request into explicit sizes, fill gaps
with common furniture sizes (18 mm boards, 720–750 mm desk height, 450 mm seat height, 600 mm
wardrobe depth) and say what it assumed, check its work by measuring and rendering, and finish
with a short summary in your language.

## The AI Assistant palette

Type AI or ASSISTANT, or choose Tools ▸ AI Assistant. The first time, the palette floats by the
right edge of the drawing; afterwards it comes back where you closed it. It docks, floats and
closes like the other palettes.

| Part | What it does |
|---|---|
| Header | The provider and model. The gear opens Options ▸ AI. **New chat** clears the chat and what the model remembers (not while a message is being answered); the drawing is left as it is |
| Examples | An empty chat offers example requests in the interface language. A click puts one in the input to edit; nothing is sent until you send it |
| Input | Enter sends; Shift+Enter starts a new line. While the input has the keyboard, typing never reaches the command line |
| **Send**, **Stop** | Stop ends the answer at once: while it streams, while the server hasn't answered yet, between retries, or before the next tool call. A tool already running finishes, and what was done stays in the drawing; a reply cut off mid-stream isn't remembered |
| Your messages | Shaded boxes |
| Answers | The model's text, with paragraphs, headings, bullets and code. Images it writes in Markdown are left out: renders show under their rows |
| Thinking… | The model's summarised reasoning, collapsed. Claude models with adaptive thinking send it, and so do local servers that stream reasoning |
| Tool rows | One per tool call: `…` while it runs, `✓` done, `✗` failed, with what it worked on (`✓ execute · panel`, `✓ render · iso`). A click shows the arguments and the result (its first 4,000 characters) |
| Renders | A render's image appears under its row |
| Notices and errors | Retries, stops and model switches in italics; failures in the warning colour. Cadino's own messages are in the interface language; a provider's come as it sends them |
| Tokens | After each message: tokens sent, received and read from the cache, when the provider reports them |

With no model set up (or, for Claude and GPT, no key), the palette says "No model is set up yet"
and offers **Set up a model…**.

The model remembers the whole chat until New chat, until you switch provider or model or change
the custom instructions (the next message starts afresh, and the chat says so), or until Cadino
closes. One message runs at most 40 rounds of tool calls; then the assistant stops with "Stopped
after 40 tool rounds; send a message to continue."

## Setting up a model (Options ▸ AI)

Open Options (OPTIONS, or Tools ▸ Options…) and the AI tab. The **Built-in assistant** section:

| Setting | What it does |
|---|---|
| Provider | Anthropic Claude (the default), OpenAI GPT, or Local / custom (OpenAI-compatible) |
| Server address | Local / custom only: the server's OpenAI-compatible address, `http://…` or `https://…`, with preset buttons |
| Model | Type a name, or pick one from the list beside it: the suggested models plus those Refresh models found. Each provider keeps its own model |
| Refresh models | Asks the provider for its models and adds them to the list ("12 models") |
| API key | Saved in the system keychain; **API key (optional)** for Local / custom. See [API keys](#api-keys) |
| Thinking effort | Anthropic only: `low`, `medium`, `high` (the default), `xhigh` or `max` |
| Allow the assistant to open, save and export files | Off by default; see [File access](#file-access) |
| Custom instructions | Added to every conversation, up to 8,000 characters, e.g. "Units are millimetres; walls are 200 thick." Changing them starts a new chat |
| Test connection | Lists the models with this address and key: "Connected: 12 models", or what failed |

A text field takes effect when it loses the keyboard focus, so a half-typed value is never used.

### Anthropic Claude

The default model is `claude-opus-5-5`; the list suggests `claude-opus-5-5`,
`claude-sonnet-5-5`, `claude-haiku-5-5` and `claude-fable-5-1`. The key comes from the keychain
or `ANTHROPIC_API_KEY`.

- **Thinking effort.** Models with adaptive thinking (the Claude 5 family, Opus 4.6–4.8 and
  Sonnet 4.6) think before they act; effort sets how much. Higher effort is slower and uses more
  tokens, and does better on long multi-part jobs. It isn't sent to older models.
- **Prompt caching.** The part of each request that doesn't change (tools, instructions, earlier
  turns) is cached by Anthropic. The tokens line shows how much was read from the cache.
- **Server-side fallback.** With `claude-opus-5-5`, `claude-opus-5`, `claude-fable-5-1` and
  `claude-sonnet-5-5`, Cadino asks Anthropic to retry a request the model declines on another
  model. The answer then comes from that model; the chat doesn't say which. If the request is
  declined anyway, the chat shows "Claude declined this request".

### OpenAI GPT

The default model is `gpt-5`; the list suggests `gpt-5` and `gpt-5-mini`. The key comes from the
keychain or `OPENAI_API_KEY`. Thinking effort is not sent.

### Local models and other OpenAI-compatible servers

The Local / custom provider talks to any server with OpenAI's Chat Completions API (streaming,
`tools`, and `/models`), usually a model running on your own computer. A local server needs no
key; a hosted one that wants a key (OpenRouter, Groq, DeepSeek…) takes it in **API key
(optional)**, sent as a bearer token. The presets fill in each server's usual address:

| Preset | Address |
|---|---|
| Ollama | `http://localhost:11434/v1` (the default) |
| LM Studio | `http://localhost:1234/v1` |
| llama.cpp | `http://localhost:8080/v1` |
| vLLM | `http://localhost:8000/v1` |

There is no default model: start the server, click **Refresh models**, and pick one.

**Choose a model that can call tools.** The assistant works only through tool calls (function
calling). A model without them fails at once; Ollama answers "HTTP 400: … does not support
tools". `ollama show <model>` lists `tools` under Capabilities: `qwen2.5:7b-instruct` has it,
the vision models `qwen2.5vl:3b` and `qwen2.5vl:7b` don't. llama.cpp's server needs `--jinja` for
tool calls, and vLLM needs `--enable-auto-tool-choice` with the model's `--tool-call-parser`.

**Images.** Renders reach only a model that takes images. A text-only model doesn't see them:
Ollama drops them silently. A server that refuses them gets none for the rest of the chat, and
the chat says "This model does not take images; the assistant goes on without them."

**Context size.** Local servers often run a small context, commonly 4,096 or 8,192 tokens on
Ollama. A local session starts at about 2,200 tokens (instructions and tools, measured on Ollama
with Qwen 2.5). When a chat outgrows the context, Ollama drops the oldest turns and keeps the
instructions, which is harmless. When the newest message alone overflows it (one large tool
result), the server cuts the *start* of the prompt: the instructions and the tool format go with
it, and the model rambles or writes its calls as text. Cadino cuts a local model's tool results
at 8,000 characters to prevent that, but a larger context still helps. Cadino can't raise it per
request (Ollama's OpenAI endpoint ignores `num_ctx`), so set it on the server:

| Server | Setting |
|---|---|
| Ollama | `OLLAMA_CONTEXT_LENGTH` before the server starts: `OLLAMA_CONTEXT_LENGTH=16384 ollama serve`, or `-e OLLAMA_CONTEXT_LENGTH=16384` for Docker |
| LM Studio | The context length when the model is loaded |
| llama.cpp | `--ctx-size 16384` |
| vLLM | `--max-model-len 16384` |

8,192 tokens is the useful minimum; use 16,384 or more for longer chats if memory allows. A
larger context needs more memory and is slower on a CPU.

**What the Local / custom provider gets differently**, to fit a local model's small context:

- A compact system prompt, about a quarter of the cloud one.
- No `script`, `screenshot`, `ui_inspect` or `ui_click` tools.
- Tool results cut at 8,000 characters (30,000 for Claude and GPT), with a note asking the model
  to ask for less: a filter, a window, a limit.
- Tool calls the model writes as text (a server that lost its tool format, a garbled first
  token) are recovered and run, also next to real calls: JSON call objects, bare, fenced or in
  `<tool_call>` tags, and `tool {arguments}`. A written call that repeats a real one (the same
  tool and arguments) runs once. Text that may be a call is held back while it streams.
- Garbled tags are dropped: a lone word on its own line (one Latin or Cyrillic word of at most 16
  letters, no punctuation, such as `ampoline` or `isphere`) right before or after a call never
  shows in the chat or reaches the history. A tool's name on its own line before a call stays
  that call's name. Lines in other scripts (완료, 完成) and lines with punctuation ("Done.") stay.
- A change the model already made in the same message isn't made again. When it repeats a call
  that changed the drawing (the same tool and arguments, for example a second identical `box`),
  the call doesn't run and the model is told the first one's result. Looking (zoom, render,
  inspect, measure), answers to a running command's prompts, failed or refused calls and other
  arguments run as usual; the latest call of a command replaces its earlier ones, so a setting
  can be set back (`layer.current` Walls, Doors, Walls). After undo (UNDO, U, REDO, MREDO, typed
  or JSON), a new drawing or an opened one, the same change can be made again. A new message
  starts clean. Claude and GPT keep track of their own calls, so their repeats always run.
- A server that isn't running fails at once with "Can't reach …", and one that says nothing for
  ten minutes (a CPU-only model can read a long prompt that long) isn't sent the request again.

**What to expect.** In our tests `qwen2.5:7b-instruct` on Ollama made a real tool call for "Draw
a circle with radius 5 centred at 0,0" 5 times in 5, and drew the right circle 4 times in 5 (the
miss had radius 50). A 7B model handles short drafting requests; multi-part 3D work (a carcass, a
loft) wants a larger model or a cloud one. Check the sizes it reports.

### API keys

- Paste the key in **API key** and click **Save**. It goes to the system keychain (Windows
  Credential Manager, macOS Keychain, the Secret Service on Linux and the BSDs) under the service
  "Cadino", one entry per provider (`anthropic`, `openai`, `custom`). The field is cleared and a
  saved key is never shown again. **Remove** deletes it.
- Or set `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` in the environment. A key in the keychain wins.
  Local / custom uses only its own saved key, never `OPENAI_API_KEY`.
- The line under the field says where the key comes from: "Saved in the system keychain",
  "Using ANTHROPIC_API_KEY from the environment", or "Not set".
- The key is read when a message is sent. It is never part of OPTIONS or the saved preferences.

### File access

**Allow the assistant to open, save and export files** is off by default. While it is off:

- The `open` and `save` tools are refused. They stay offered, since a chat's tools never change.
- These commands are refused, by id, alias (Rhino names and your own included) or typed: OPEN,
  QSAVE, SAVEAS, RECOVER, DRAWINGRECOVERY, SCRIPT, REINIT, IMPORT, EXPORT, WBLOCK, PLOT,
  EXPORTPDF, PUBLISH, PDFBOOK, STEPIN, STEPOUT, STLOUT, IGESIN, IGESOUT, RHINOOUT, XATTACH, XREF
  and -XREF, XOPEN, ATTACH, DATALINK, APPLOAD, LISP, ALIASEDIT (a new alias could rename one of
  them), and File ▸ Open Recent's items.
- A call with a parameter named `path`, `file`, `csv`, `template`, `patternFile` or
  `savefilepath`, at any depth, is refused: INSERT from a file, CUTLIST's CSV, NEW from a
  template, COMPARE with a file, OPTIONS's SAVEFILEPATH, a render written to disk. SWEEP, ARRAY
  and ARRAYPATH keep their `path`, which is a curve.
- At the command line, a line is refused when its first input is one of those commands, or it
  starts a transparent `'command`, a LISP expression `(…)` at the start of the line, or a
  `command {json}` with a file parameter. In a script, any later input of two letters or more
  that names one counts too.
- Behind these checks, Cadino itself refuses every file read and write while it serves the
  assistant, whatever leads there: a file name typed at a running command's prompt (LINETYPE's
  load, HATCH's pattern file, -INSERT), a script, LISP, or a render or screenshot saved to a path.

A refused call answers "File access is off for the assistant (Options ▸ AI)", and the model is
told to tell you rather than work around it. The ceilings:

- In a script the checks can refuse harmless text, such as a TEXT line containing "save".
- It covers the built-in assistant only. External AI apps over MCP can always open and save.

## External AI apps (MCP)

The **External AI apps (MCP)** section of Options ▸ AI lets Claude Desktop, Claude Code, Codex
CLI, Cursor and other MCP clients drive the running Cadino, so you can watch them work.

| Setting | What it does |
|---|---|
| Let AI apps on this computer control Cadino | Starts a control server on `127.0.0.1`. It is remembered, so it starts again with Cadino |
| Port | 1024–65535, 7979 by default |
| Status | "Off", "Listening on 127.0.0.1:7979", "Port 7979 is in use" or "Port 7979 could not be opened: …" |

> [!WARNING]
> While this is on, any program on this computer can control Cadino: draw, erase, open, save
> and overwrite files, and quit. The server listens on the loopback address only and has no
> password. Web pages can't use it: a line that isn't JSON, such as a browser's request, ends the
> connection. Turn it on when you use an AI app and off afterwards.

Below the switch are ready-to-copy configurations, each with a **Copy** button, filled in with
the `cadcraft-cli` next to Cadino and the port. If `cadcraft-cli` isn't in Cadino's folder, the
section says so; build or install it there. With `--lazy` the client can start before Cadino: its
calls fail with a message until Cadino runs with the switch on.

**Claude Desktop** (`claude_desktop_config.json`; Settings ▸ Developer ▸ Edit Config) and
**Cursor** (`.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "cadino": {
      "command": "/path/to/cadcraft-cli",
      "args": ["mcp", "--connect", "127.0.0.1:7979", "--lazy"]
    }
  }
}
```

**Claude Code**, in a terminal:

```sh
claude mcp add cadino -- '/path/to/cadcraft-cli' mcp --connect 127.0.0.1:7979 --lazy
```

**Codex CLI** (`~/.codex/config.toml`):

```toml
[mcp_servers.cadino]
command = "/path/to/cadcraft-cli"
args = ["mcp", "--connect", "127.0.0.1:7979", "--lazy"]
```

**ChatGPT.** The ChatGPT app connects only to remote HTTPS MCP servers, so it can't reach this
one. Use Codex CLI, or the built-in assistant with OpenAI GPT.

External apps get the same tools as the assistant with Claude or GPT (`screenshot`, `ui_inspect`
and `ui_click` included) and short working instructions when they connect. The assistant's limits
(file access, settings, closing, your running command) don't apply to them.
`cadcraft --control 7979` on the command line starts a control server for one run instead of the
switch (both on one port: "Port 7979 is in use"), and `cadcraft-cli mcp` runs a headless drawing
with no window. See [mcp.md](mcp.md).

## OPTIONS keys

The AI settings are part of the OPTIONS command's JSON form, like every other tab:
`options {"aiProvider": "custom", "aiBaseUrl": "http://localhost:11434/v1", "aiModel": "qwen2.5:7b-instruct"}`.
A call changes all of its keys or none; `aiProvider` is applied first, so `aiModel` goes to the
provider chosen in the same call. `"reset": true` leaves the AI settings as they are.

| Key | Value |
|---|---|
| `aiProvider` | `"anthropic"`, `"openai"` or `"custom"` (an OpenAI-compatible server, usually a local model) |
| `aiModel` | The chosen provider's model, e.g. `"claude-opus-5-5"`; empty: the provider's default |
| `aiBaseUrl` | The custom provider's server, `"http://…"` or `"https://…"` (default `"http://localhost:11434/v1"`) |
| `aiEffort` | Claude's thinking effort: `"low"`, `"medium"`, `"high"`, `"xhigh"` or `"max"` |
| `aiCustomInstructions` | Added to the assistant's instructions, at most 8,000 characters |
| `aiAllowFiles` | `true` lets the assistant open, save and export files |
| `mcpEnabled` | `true` runs the control server external AI apps connect to |
| `mcpPort` | Its port, 1024–65535 (default 7979) |

API keys are never part of OPTIONS. The built-in assistant can't change the `ai…` and `mcp…`
keys.

## Privacy

What goes to the provider you chose (Anthropic, OpenAI, or the server at the address you set,
which for a local server is your own computer), with every request:

- The system prompt and your custom instructions.
- Your messages and the whole chat so far.
- What the tools return: drawing data such as geometry and handles, layer, block and layout
  names, texts and labels, units and the command catalog, and render images (PNG). A 3D solid's
  B-rep and mesh are replaced by their sizes; its `bbox3` is sent.
- The API key, to that provider only: redirects are not followed, so it never reaches another
  host.

The drawing file itself is not uploaded; the model sees only what the tools return.

What Cadino keeps: the settings, with the interface preferences, and the keys, in the system
keychain. The chat is held in memory only, until New chat or until Cadino closes, and Cadino's
log doesn't record it. What the assistant drew is part of the drawing and its undo history, like
your own work. What a provider keeps is governed by its terms; with a local server nothing leaves
your computer. An external AI app sends what it reads from Cadino to its own model, under its
own terms.

## How it works

- **Tools.** The assistant's tools are the MCP tools ([mcp.md](mcp.md)): `command_line`, `script`,
  `execute`, `list_commands`, `inspect_drawing`, `query_entities`, `find_text`, `render`,
  `new_drawing`, `open`, `save` and `cancel`, and with the window `screenshot`, `ui_inspect` and
  `ui_click`. In the app each call reaches Cadino as a control request served between frames,
  the same path an external MCP client takes, so the window stays live while the assistant works.
- **The loop.** Your message goes to the model with the chat so far. Its reply streams in; the
  tool calls it makes run in order on the open drawing, and their results (text and images) go
  back to it. This repeats until it answers without calling a tool, at most 40 rounds. When a
  message that changed the drawing ends with part of it out of view, Cadino zooms to extents
  once, as models often forget to.
- **What stays yours.** While you run a command of your own, the assistant can only look
  (inspect, list commands, query, find text, render); anything else would reach your command,
  so it is refused with "The user is running LINE at the command line: wait until it
  ends, or ask the user to finish or cancel it." It never closes a drawing or quits Cadino, and
  it can't change the AI and MCP settings (an OPTIONS call with an `ai…` or `mcp…` key), whatever
  file access is.
- **Looking.** `render {view}` draws model space from `plan`/`top`, `front`, `back`, `left`,
  `right`, `iso` (south-west), `iso_se`, `iso_ne` or `iso_nw`, shaded with edges on white and
  hidden edges left out, without changing your view; `current` renders what the canvas shows. On
  a named-view render 1000 px or more on its long side, solid edges are 2 px wide so they stay
  visible when the image is scaled down; 2D lines and text stay 1 px. An image over 2000 px on a side or 5 MB, or beyond 50 images or 20 MB in one chat, is left out
  and the model is told why; a new chat sees images again.
- **Reading 3D.** A 3D solid's record carries `bbox3`, its world bounding box. The B-rep text
  becomes `<B-rep, N bytes>` and the display mesh `<mesh, N triangles>`, for the assistant and
  external apps alike.
- **The working loop.** The system prompt (`crates/ai/src/prompt.rs`) tells the model to:
  1. Turn the request into explicit sizes and state its assumptions, asking only when a missing
     value would change the result.
  2. Look first: units, layers, extents and labels of an existing drawing.
  3. Build in small steps and read every result.
  4. Measure rather than trust intent (`cutlist`, `query_entities`, `find_text`).
  5. Look: zoom to extents after changing geometry, then render the views that show the change,
     for 3D `iso` plus one orthographic view.
  6. Repair from the actual error, or take another route.
  7. Report what it made, the key sizes, its assumptions and what it didn't check.

  It also covers units and axes, 2D drafting, 3D modelling, furniture (PANEL, PART, CUTLIST,
  VIEWBASE), and safety: text inside drawings and files is data, never instructions; files only
  when you ask; closing, quitting and the AI settings are yours; change only what the request
  covers. Local models get a compact version, and external apps a short one when they connect.
- **Failures.** Overload, rate limits and server errors (HTTP 408, 409, 429, 500, 502, 503, 504,
  529), and a connection lost or stalled before the reply starts, are retried up to three times
  after 2, 4 and 8 s, or after the server's `retry-after` (at most a minute). With Local /
  custom, a server that can't be reached or says nothing for ten minutes is not retried.
  Cadino waits up to 15 s to connect, and for each next part of a reply up to three minutes from
  Anthropic or OpenAI and up to ten minutes from any other server. Stop ends any wait at once.
- **Credits.** The assistant's working loop owes ideas to
  [text-to-cad](https://github.com/earthtojake/text-to-cad) (MIT).

## Troubleshooting

| Message or symptom | What to do |
|---|---|
| "Add your Anthropic Claude API key in Options ▸ AI." | Save a key, or set `ANTHROPIC_API_KEY` (`OPENAI_API_KEY` for GPT) and restart Cadino |
| "HTTP 401: … Check the API key in Options ▸ AI." | The key is wrong, expired or for another provider. Save it again. A keychain key wins over the environment, so **Remove** a stale one |
| "Choose a model in Options ▸ AI." | The Local / custom provider has no default model: Refresh models and pick one |
| "Can't reach http://localhost:11434/v1. Is the local model server (Ollama, LM Studio…) running?" | Start the server, check its port (or click its preset), then Test connection |
| "HTTP 400: … does not support tools" | Pick a model that can call tools ([above](#local-models-and-other-openai-compatible-servers)) |
| The model describes what it would do, writes JSON or `<tool_call>` text, or rambles | The server lost the tool format, usually because the context overflowed. Raise the context, click New chat, and ask for one step at a time. Calls written as text are run when Cadino can read them; one whose tool name was garbled can't be |
| A local model is very slow | It is probably running on the CPU. Use the GPU, a smaller model or a smaller context. Stop ends a message at any time |
| The model doesn't react to renders | It doesn't take images; renders are left out or ignored |
| "… Trying again in 4 s (2/3)" | The provider is overloaded or rate-limited; Cadino retries by itself |
| "Claude declined this request" | Anthropic's server-side fallback already tried another model. Rephrase the request, or pick another model |
| "The reply reached the output limit (32000 tokens); ask for a smaller step or to continue." | One Claude reply hit the 32,000-token limit; its tool calls didn't run. Ask for the work in smaller steps, or send "continue" |
| "The reply reached the model's length limit." | A GPT or local reply was cut off; its tool calls didn't run. Ask for smaller steps |
| "Stopped after 40 tool rounds; send a message to continue." | Send "continue", or split the job |
| "Switched to …: the conversation starts over." | You changed the provider or model; the new model doesn't see the old chat |
| "The custom instructions changed: the conversation starts over." | You edited the custom instructions; the next message starts a new chat with them |
| A ✗ row: "The user is running LINE at the command line…" | Finish or cancel your command (Esc); until then the assistant can only look |
| "File access is off for the assistant (Options ▸ AI)." | Turn on file access, or open and save the file yourself |
| "Port 7979 is in use" | Another program, another Cadino or a `--control 7979` run has the port. Choose another port and copy the configurations again |
| An external app can't reach Cadino | Cadino must be running with the switch on and the same port as in the app's configuration |
| "The AI assistant needs the desktop app" | The web build has no assistant and no MCP switch |

## Known ceilings

- Desktop only: the web build has neither the assistant nor the MCP switch.
- A Claude reply stops at 32,000 output tokens; Options ▸ AI doesn't change it.
- A hosted server under Local / custom is treated like a local one: the compact prompt, fewer
  tools, results cut at 8,000 characters, and no retry when it can't be reached.
- The palette, its notices and Cadino's own errors are translated into Korean and Ukrainian so
  far; other interface languages show them in English.
- Reading calls written as text is a heuristic, applied to every Local / custom reply, also next
  to real calls: a word ending in a tool name right before a JSON object runs that tool, and so
  does a JSON object naming a tool with its `arguments` or `parameters`. Replies over 64 KiB
  aren't scanned.
- Dropping garbled tags is a heuristic too: a one-word English line right beside a call
  ("Okay") goes with them, and a word piece in text that streams after the server's structured
  calls stays (servers send those calls last).
- The repeated-call guard compares the tool and its arguments only. A local model that means to
  make the same object twice in one message (two identical copies) gets the second refused until
  it changes an argument; split such work across messages.
- File access has the limits listed under [File access](#file-access).
