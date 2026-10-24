# File-format parity

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-24 · **Change:** minor (raster images attached and drawn, in PDF/SVG too, eTransmit packages, partial open, dynamic-block authoring and its `CADCRAFT_DYNBLOCK` record, perspective, twisted and clipped VPORT records) · **Target:** Autodesk AutoCAD 2027

Every format AutoCAD 2027 reads or writes, with CADCraft's support, fidelity and tests. Work
items are in [gaps.md](gaps.md#file-formats); the weighted score feeds
[target-app-parity.md](target-app-parity.md).

**Headline (estimated):** file formats ≈ **30%** of AutoCAD's. Opening the commonest 2D DXF/DWG
drawings works, but saving is lossy: everything CADCraft doesn't model is dropped, and no test
uses a real third-party file. DXF and DWG save at 2000–2018 (2018 by default, #421). Remaining: **≈ 170–280 Opus 5.5 hours**:
≈ 40–60 h for DXF/DWG fidelity (the part that decides beta), ≈ 110–180 h for the other formats
below, and ≈ 20–40 h for 3D exchange formats that wait for 3D modelling.

## How this was measured

- CADCraft: read from `crates/dxf`, `crates/io` (`dxf_read.rs`, `dxf_write.rs`, `pdf.rs`,
  `svg.rs`, `tests.rs`) and `crates/dwg` on 2026-10-10. Nothing was run (the measurement pass
  did no builds).
- AutoCAD: the formats in its Open/Save As/Import/Export/Attach/Plot dialogs, from Autodesk's
  published documentation and the menu tree in `xtask/data/menu-catalog.txt`. The installed
  AutoCAD 2027 for Mac bundle was inspected by listing names only (clean-room rule: nothing
  inside it is read): `Resources/Support` lists `acad.lin`, `acadiso.lin`, `acad.pat`,
  `acadiso.pat`, `acad.mln`, `.dct` spell dictionaries, `.pss` and `.shx` files, which confirms the
  supporting formats below exist in the target. None of them may be copied.
- No AutoCAD-produced file is committed or used as a fixture. A real-file corpus needs owner
  action (see [gaps.md](gaps.md)): files made by people who license them to us, kept in a
  separate `storytold/cadcraft-corpus` repo.

## DWG, version by version

AutoCAD 2027 opens every DWG version from R2.0 onward and saves 2018 (AC1032, still the current
DWG format), 2013, 2010, 2007, 2004, 2000 and R14. CADCraft reads DWG by converting it to DXF
with the `acadrust` 0.6.3 crate and reading that DXF; it saves by writing its own R2000 DXF and
converting it to a DWG of the chosen version (#421): SAVEAS `version`, else the drawing's own
version (the file's `$ACADVER`, 2018 for a new drawing), at least 2004 unless 2000 is asked for.
The bridge repairs two acadrust 0.6.3 gaps in 2010+ DWG: merged table cells and table-style
title/header suppression.

| DWG version | Code | AutoCAD 2027 | CADCraft read | CADCraft write | Tested how |
|---|---|---|---|---|---|
| R1.0–R2.6, R9, R10, R11/R12 | MC0.0–AC1009 | read | **no** (`is_dwg` accepts only `AC10xx` magic; acadrust doesn't target pre-R13) | no | — |
| R13 | AC1012 | read | acadrust claims support; untested | no | none |
| R14 | AC1014 | read, write | acadrust claims support; untested | **no** | none |
| 2000–2002 | AC1015 | read, write | yes via bridge | yes, when asked for (no true colours, transparency or table styles: the format has none) | synthetic round trips (`dwg_at_every_version_reads_back`, `dwg_roundtrip_keeps_non_ascii_text`) |
| 2004–2006 | AC1018 | read, write | via bridge | yes | `dwg_at_every_version_reads_back`, `dwg_roundtrip_keeps_extension_data` |
| 2007–2009 | AC1021 | read, write | via bridge | yes | `dwg_at_every_version_reads_back` |
| 2010–2012 | AC1024 | read, write | via bridge | yes | `dwg_at_every_version_reads_back`, `ac1024_dwg_with_many_entities_opens` |
| 2013–2017 | AC1027 | read, write | via bridge | yes | `dwg_at_every_version_reads_back` |
| 2018–2027 | AC1032 | read, write (default) | via bridge; untested on real files | yes (default for new drawings) | `dwg_at_every_version_reads_back` (synthetic only) |
| Web build | — | — | **no** (error: "save as DXF") | no | — |

Everything lost in the DXF read/write paths below is also lost through DWG, plus whatever
`acadrust`'s own DXF output omits (proxy objects, ACIS data, custom objects: not measured). User
report #57: a 10 MB DWG failed after about two minutes; limits now stop runaway conversions
early (#175), but the throughput problem remains. **DWG ≈ 30% read, ≈ 15% write.**

## DXF, version by version

| DXF version | AutoCAD 2027 | CADCraft read | CADCraft write |
|---|---|---|---|
| R12 (AC1009), ASCII | read, write | yes: no version gating; old-style POLYLINE/VERTEX read (polyface meshes flattened to 3D polylines) | **no** |
| R12 binary | read, write | **no** (binary parser expects R13+ 2-byte group codes) | no |
| R13–R2018, ASCII | read, write each of 2000/2004/2007/2010/2013/2018 | yes, one reader for all versions | yes, each of 2000–2018 (#421): R2000 structure (later versions add only optional groups) with that `$ACADVER`; UTF-8 text from 2007; no CLASS instance counts (91) |
| R13–R2018, binary | read, write | yes | yes, each of 2000–2018 (SAVEAS `binary`; a binary file stays binary on QSAVE) |
| Code pages (`$DWGCODEPAGE`, ANSI_932/936/949/950/125x) | yes | yes (#505): ASCII and binary DXF before 2007 decode from their code page (ANSI_874/932/936/949/950/1250–1258, DOS437/850/852/866, ISO 8859, Macintosh); a file that is valid UTF-8 reads as UTF-8; Johab and other DOS code pages read as ANSI_1252 (`dxf/src/codepage.rs`) | writes `\U+` escapes (#169), which is correct; declares `ANSI_1252` |
| `\U+`/`\M+` escapes | yes | yes (`\M+` since #505; Johab `\M+4` stays literal) | `\U+` yes |

### DXF content: read and write by object type

| Content | Read | Write | Notes |
|---|---|---|---|
| LINE, POINT, CIRCLE, ARC, ELLIPSE, LWPOLYLINE (widths, bulges, elevation), SPLINE, RAY, XLINE | yes | yes | round-trip tests |
| POLYLINE 2D/3D | yes | yes (as LWPOLYLINE / 3D POLYLINE) | polyface and polygon meshes flattened: lossy |
| TEXT, MTEXT, ATTDEF, INSERT + ATTRIB | yes; multiline attributes (embedded MTEXT) read and drawn | yes; multiline attributes written as embedded MTEXT (DXF 2007 and later; older versions: one line) | ATTDEF prompt kept (#137) |
| DIMENSION (all 7 kinds) | yes | yes, with anonymous `*D` blocks | a reopened dimension's `*D` block is regenerated under its name, unreferenced ones dropped (#495); DIMASSOC objects written |
| LEADER | yes | yes | |
| MULTILEADER | yes (#459) | yes, native MULTILEADER with MLEADERSTYLE (#459) | several landings per multileader are folded into one; per-object overrides and annotative scale contexts not kept |
| HATCH pattern / solid | yes | yes; associative hatches keep their boundary objects in blocks too, and the boundary carries the hatch reactor (stretched dynamic blocks drag the fill in AutoCAD) | |
| HATCH gradient | yes | yes (#162, fixes #80) | |
| SOLID, TRACE, 3DFACE | yes | yes | |
| ACAD_TABLE | yes | yes (entity + block) | |
| VIEWPORT | yes | yes; frozen-layer lists kept (#150), also as standard group 341 layer handles that other programs read | DWG: the DXF→DWG converter does not yet write group 341 (frozen layers survive a CADCraft round trip via xdata); VP layer colour overrides lost (#99, PR #197) |
| IMAGE | yes, with IMAGEDEF (and its loaded flag) and RASTERVARIABLES (IMAGEFRAME, IMAGEQUALITY) | yes, with IMAGEDEF, IMAGEDEF_REACTOR and RASTERVARIABLES | PNG, JPEG and BMP pictures are read (capped, decoded once per file) and drawn on screen (perspective and clipped views too), in PNG exports and plot preview, and in PDF (an image XObject per picture, a soft mask for see-through pixels) and SVG (a PNG data URI) output |
| WIPEOUT | yes | yes (#147, fixes #79) | |
| MLINE (+ MLINESTYLE), HELIX, TOLERANCE | yes (#510, #420) | yes | |
| REGION | other programs' REGIONs (ACIS data) kept as Unknown | our regions are written as closed polylines tagged with CADCraft xdata (#510; ACIS has no public spec) | |
| 3DSOLID, BODY, SURFACE, MESH, SHAPE, UNDERLAY, OLE2FRAME, LIGHT, CAMERA, SECTION, POINTCLOUD, proxy entities | kept as Unknown (raw tags) | written back as read, with their CLASS records (#528); **dropped by DWG save** (acadrust) | only their layer follows edits |
| Header variables | all read | 36 written | AutoCAD writes about 280 |
| Tables: LAYER (+ transparency, description), LTYPE, STYLE, DIMSTYLE, BLOCK_RECORD | yes | yes | |
| Tables: VPORT, VIEW, UCS, APPID | yes | yes: saved viewport configurations (#504), named views, UCSs; a tile's twist (51) and clipping planes (43, 44, VIEWMODE bits 2, 4, 16) | UCS elevation lost |
| Objects: LAYOUT, DICTIONARY, XRECORD, TABLESTYLE, DIMASSOC | yes | yes | |
| Objects: GROUP, MLINESTYLE, MLEADERSTYLE, IMAGEDEF, SCALE | yes | yes (groups #417, scale list #468) | |
| Objects: VISUALSTYLE, PLOTSETTINGS, materials, field objects and other unmodelled objects | kept raw (#528) | written back as read | not editable |
| Foreign xdata, extension dictionaries, reactors, unmodelled objects, colour books | kept raw (#528); an entity's xdata is part of the entity, so COPY, MIRROR, ARRAY and paste keep it on the copies (EXPLODE pieces don't) | written back, handles remapped, references to deleted objects dropped | through DWG: xdata on table records and blocks, entity colour books and reactors, custom-class objects lost (acadrust) |
| Objects: dynamic block data (ACAD_EVALUATION_GRAPH, BLOCK…PARAMETER, BLOCK…ACTION, grips) | read into a model (`dynblock`, `blocks.dynamic`): parameters, visibility states, value sets, stretch and lookup actions; references: their dynamic block, values and custom properties (`blocks.reference`, Properties ▸ Custom, read-only); kept raw (#528) | written back as read; a changed visibility state is written as AutoCAD writes it (a new anonymous block with its `AcDbBlockRepBTag`, the objects' `AcDbBlockRepETag`, the reference's representation data). Blocks authored in the Block Editor (BPARAMETER, BACTION, BVSTATE, BLOOKUPTABLE) are written as AutoCAD's own objects only for the kinds whose layout follows AutoCAD's files: linear, point, visibility, lookup and base point parameters, stretch, move and lookup actions, and their grips. Polar, XY, rotation, alignment and flip parameters (with their grips) and scale, rotate, flip, array and polar stretch actions have layouts of our own, so they, and the elements that depend on them (actions following them or carrying them along, lookups reading them), go into a Cadino XRECORD `CADCRAFT_DYNBLOCK` in the block record's extension dictionary (versioned group pairs, which AutoCAD keeps without reading), as does Cadino-only data on AutoCAD's kinds (a lookup table's Allow reverse lookup); opening the file puts them back (`doc/src/dynblock/file.rs`). The record (after `100 AcDbXrecord`, `280 1`): format version 90; the elements kept (91 count; each its DXF type 2, graph node id 92, group count 93 and groups); the Cadino sections of elements written as AutoCAD's (95 count; each node id 92, count 93, groups); and each visibility state's active elements among the kept ones (96 state count; each a 97 count of 98 node ids). A group is `70 code` + `1 value`, or `71 code` + `94 node` for a reference to another element (by node id, as a file changes handles). An XY parameter's four grips go with it, and the CLASS records of the record's types no object uses any more are dropped. A graph AutoCAD wired is written as read, and a record whose block AutoCAD has changed since is dropped. A record of another (newer) version is kept as read, and its block refuses added or removed elements and lookup table changes, so it is never replaced. An array action's copies on a reference carry `CADCRAFT_DYNARRAY` xdata. Editing a parameter rewrites only the groups modelled; a graph AutoCAD wired (edges) refuses added or removed elements | visibility states (and lookups that set only them) can be changed (`blocks.reference.set`, Properties ▸ Custom); distances, positions and the actions they drive not yet; DWG open and save keep it too (checked with an AutoCAD dynamic block) |
| Handles and owners | entity handles kept; others regenerated | consistent | stable across CADCraft re-saves (#495); other programs' table and object handles renumbered once |

**DXF ≈ 50% read, ≈ 30% write** (estimated; weights: common 2D entities 50%, annotation objects
20%, tables/objects 15%, foreign data preservation 15%).

## Other formats

| Format | AutoCAD 2027 | CADCraft | Hours to parity (est.) |
|---|---|---|---|
| DWT templates | open, save | NEW/QNEW from `.dwt` and `.dxf` templates, SAVEAS `.dwt` (#552) | 0–1 |
| DWS standards | open, save, CHECKSTANDARDS | no | 6–10 |
| PDF output (PLOT, EXPORTPDF) | multi-sheet, layers, searchable text, hyperlinks | vector PDF with lineweights, real text in embedded TrueType subsets, layers (OCG), several sheets per file and hyperlinks (#567, #571); object transparency as PDF alpha when "Plot transparency" is on (the option isn't saved in DXF/DWG: no public plot layout flag) | 1–2 |
| PDF import (PDFIMPORT) and PDF underlays (PDFATTACH) | yes | no | 15–25 |
| Raster images attach (PNG, JPEG, TIFF, BMP …) | yes | PNG, JPEG and BMP: IMAGEATTACH/ATTACH, -IMAGE, IMAGECLIP, IMAGEADJUST, IMAGEFRAME, IMAGEQUALITY, TRANSPARENCY and the External References palette; pictures in PDF and SVG output; no TIFF, no web attach | 2–4 |
| PNG / JPEG / BMP / TIFF export | PNGOUT, JPGOUT, BMPOUT, TIFOUT | PNG; SVG too (AutoCAD doesn't export SVG) | 1–2 |
| DWF/DWFx export, DWF underlay | yes | no | 10–15 |
| DGN import/export/underlay | yes | no (needs a public spec; owner decision) | 25–40 |
| WMF, EPS, ACIS SAT, STL, FBX, IGES/STEP (Windows), 3DS | various | no; most are 3D and wait for M11 | 20–40 |
| SHX fonts (`.shx`, big fonts) | read | **not parsed**: our stroke font or FONTALT stands in, so text metrics differ from AutoCAD | 8–12 |
| Linetype (`.lin`) and hatch pattern (`.pat`) files | load user files | user files load (#552); linetype shape (SHX) elements are skipped | 1–2 |
| Plot styles (`.ctb`, `.stb`), plotter configs (`.pc3`), `.pmp` | yes | plot style tables in our own JSON format ([plot-styles.md](plot-styles.md)) and kept in the drawing; plotter devices with papers and printable areas in our own JSON format ([plotters.md](plotters.md), #564); binary `.ctb`/`.stb`/`.pc3`/`.pmp` not read (no public spec) | 4–8 |
| Scripts (`.scr`) | SCRIPT | script text through the CLI, MCP and control channel; no SCRIPT command reading a `.scr` file | 1–2 |
| Aliases (`.pgp`), CUI(x), workspaces | yes | user alias files and ALIASEDIT (#552); no CUI(x) or workspaces | 4–8 |
| SVG import (`.svg`, `.svgz`) | yes (via scripts) | OPEN reads shapes, paths, groups and Inkscape layers into entities (#636); no CSS classes, `use`, fills, text or images | 2–4 |
| AutoLISP (`.lsp`, `.fas`, `.vlx`), `.dll`/`.arx` plug-ins | yes | `.lsp` loads (APPLOAD, `(load)`, a dropped file; #758); no Visual LISP ActiveX (`vlax-`), DCL dialogs or reactors; `.fas`/`.vlx` are compiled and undocumented; no `.dll`/`.arx` | see [gaps.md](gaps.md) (E1) |
| Data extraction (`.csv`, `.xls`) and data links | yes | DATAEXTRACTION/EATTEXT to CSV or a table (#433); table data links read CSV (#534); no `.xls`, no `.dxe` templates | 3–5 |
| eTransmit (`.zip` packages) | yes | ETRANSMIT/-ETRANSMIT: the drawing with its xrefs, images, fonts and plot style tables in a stored ZIP and a transmittal report; same-named files are kept apart and the packaged drawing points at them; first-level references only, no Create Transmittal dialog, no ZIP64 | 1–2 |

## Tests

`crates/io/src/tests.rs` has 36 tests (round trips of entities, styles, associativity, tables,
constraints, non-ASCII text, DWG round trips, hostile input). **All inputs are synthetic**:
inline strings or our own writer's output. No test opens a file made by another program, and no
external validator runs in CI (ezdxf's audit was run by hand once, 2026-10-06). The next step is
a licensed real-file corpus with an open → save → re-open → compare test per file, and checking
our output in AutoCAD black-box.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-24 | minor | `CADCRAFT_DYNBLOCK` record layout documented; an XY parameter's four grips go into it, CLASS records of types no object uses are always dropped, a newer version's record is never replaced; VPORT twist (51), front and back clipping planes (43, 44) and VIEWMODE bits 2, 4, 16 read and written; pictures in PDF (image XObjects with a soft mask) and SVG (PNG data URIs) output |
| 2026-10-24 | minor | Dynamic blocks made here: only kinds with AutoCAD's own layout are written as AutoCAD objects; polar/XY/rotation/alignment/flip parameters and scale/rotate/flip/array/polar stretch actions go into the `CADCRAFT_DYNBLOCK` XRECORD and are read back |
| 2026-10-24 | minor | Raster images attached, read and drawn (IMAGE rows; FF10 narrowed); ETRANSMIT packages; PARTIALOPEN/PARTIALOAD; dynamic blocks authored in the Block Editor written as AutoCAD's objects (unchecked layouts noted); VPORT perspective records (VIEWMODE 71 bit 1, lens 42, target 17, camera along 16) |
| 2026-10-12 | minor | PDF output: object transparency plotted as PDF alpha (`/ExtGState` `/ca` `/CA`) when the page setup's or the plot's "Plot transparency" is on; PNG plots blend it, SVG uses `opacity`; plots without it are unchanged byte for byte |
| 2026-10-12 | minor | DXF/DWG save: the Model layout's plot settings are written as CADCraft plots model space, drawing extents scaled to fit (PLOTSETTINGS 74 = 1, 75 = 0), instead of plot type 5 "Layout information", which other programs' Plot dialogs can't use for model space |
| 2026-10-12 | minor | VIEWPORT frozen layers also written and read as AutoCAD's group 341 layer handles, so third-party viewers honour per-viewport freezing in DXF (DWG converter gap noted) |
| 2026-10-12 | minor | HATCH: associative hatches inside blocks keep their source objects; boundaries carry `{ACAD_REACTORS}` |
| 2026-10-12 | minor | Dynamic block grips: grip objects read by graph id; a drag is saved as AutoCAD saves a Properties change |
| 2026-10-12 | minor | Dynamic block references: visibility states can be changed and are saved as AutoCAD saves them |
| 2026-10-12 | minor | Dynamic block references: definition, values and custom properties read (`blocks.reference`, Properties ▸ Custom) |
| 2026-10-12 | minor | Dynamic block data row: parameters and actions read into a model (`blocks.dynamic`); kept by DXF and DWG save |
| 2026-10-12 | minor | Multiline attributes (embedded MTEXT) read, drawn and written back instead of losing their text |
| 2026-10-11 | minor | Rows refreshed for merged work: code pages and `\M+` (#505), MULTILEADER (#459), IMAGE, MLINE/HELIX/REGION/TOLERANCE, VPORT/VIEW/UCS tables, modelled and raw objects, foreign data (#528), DWT templates and user `.lin`/`.pat`/`.pgp` files (#552), PDF output (#567, #571), plotter devices (#564), data extraction (#433, #534); weighted percentages not re-measured |
| 2026-10-11 | minor | DXF (ASCII and binary) and DWG saved at 2000–2018, 2018 by default (#421) |
| 2026-10-10 | minor | WIPEOUT and gradients written since #147/#162; other save losses remain (#201, #204, #208, #209, #213, #223) |
| 2026-10-10 | major | First file-format parity doc: DWG and DXF version by version, per-object read/write, other formats |
