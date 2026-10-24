Cadino
======

Cadino is a computer-aided design application for Windows: AutoCAD-style 2D drafting and
Rhino-style 3D modelling (on the OpenCASCADE kernel) in one program, working on DXF and DWG
drawings. It is based on CADCraft by the ArtCraft team.

Starting it
-----------
- Installed with the MSI: Start menu > Cadino, or right-click a .dxf or .dwg file >
  Open with > Cadino.
- Portable zip: run Cadino.exe in this folder. Keep cadcraft-cli.exe next to it.
- From a terminal:
    Cadino.exe [DRAWING.dxf|DRAWING.dwg] [--sample] [--control PORT]
    cadcraft-cli.exe            (prints its commands: info, convert, run, mcp)

AI and MCP
----------
- The built-in AI Assistant: Tools > AI Assistant. Choose the provider, model and API key in
  Options > AI.
- External AI apps (Claude Desktop, Claude Code, Codex CLI, Cursor): Options > AI >
  "Let AI apps on this computer control Cadino" starts the control server (port 7979 by
  default) and shows ready-to-copy configurations. They run cadcraft-cli.exe from this folder:
    cadcraft-cli.exe mcp --connect 127.0.0.1:7979 --lazy
- Without a window (a headless drawing session): cadcraft-cli.exe mcp

Documentation and source code
-----------------------------
Cadino's complete source code for this build is Cadino-<version>-source.zip, published next to
the installer and the portable zip (its SHA-256 is in the same SHA256SUMS.txt). It also holds the
Open CASCADE Technology sources this build links (see THIRD-PARTY-NOTICES.txt) and docs/release.md,
which explains how to build it. The documentation is in its docs folder:
  docs/ai.md                 AI Assistant, models, MCP switch, privacy
  docs/mcp.md                the MCP tools
  docs/3d.md                 3D modelling
  docs/control-protocol.md   driving the app over its control port
  docs/autolisp.md           AutoLISP
  docs/plotters.md           plotting

Files
-----
  Cadino.exe                 the application
  cadcraft-cli.exe           command line and MCP server
  LICENSE-MIT, LICENSE-APACHE, NOTICE
                             Cadino's licence (MIT OR Apache-2.0) and the upstream notices
  THIRD-PARTY-NOTICES.txt    Open CASCADE Technology, the Rust crates and fonts in this build,
                             with their licences
