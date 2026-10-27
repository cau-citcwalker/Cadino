# Cadino recipes

Every line below is one call, checked on Cadino 0.7 with the 3D kernel. A line such as `panel {…}` is
`execute {"command": "panel", "params": {…}}`; lines marked `tool:` are MCP tools called directly.
`<name>` stands for the handle an earlier result returned (`# → <name>`). Sizes are millimetres.

## Cabinet 600 × 400 × 750, 18 mm carcass, 6 mm back, one overlay door

Sides at full height and depth; bottom, top and shelf between the sides (inner width
600 − 2 × 18 = 564) and 394 deep so the back fits behind them; the back between the sides and
between top and bottom; the door in front with 2 mm gaps.

```
new {"metric": true}
# only for a new model; otherwise check getvar {"name": "INSUNITS"} is 4
layer.new {"name": "CARCASS", "color": "cyan", "current": true}
panel {"corner1": [0, 0], "corner2": [400, 750], "orientation": "right", "at": [18, 0, 0], "name": "Left side", "material": "Birch ply 18"}
panel {"corner1": [0, 0], "corner2": [400, 750], "orientation": "right", "at": [600, 0, 0], "name": "Right side", "material": "Birch ply 18"}
panel {"corner1": [0, 0], "corner2": [564, 394], "at": [18, 0, 0], "name": "Bottom", "material": "Birch ply 18"}
panel {"corner1": [0, 0], "corner2": [564, 394], "at": [18, 0, 732], "name": "Top", "material": "Birch ply 18"}
panel {"corner1": [0, 0], "corner2": [564, 394], "at": [18, 0, 366], "name": "Shelf", "material": "Birch ply 18"}
panel {"corner1": [0, 0], "corner2": [564, 714], "thickness": 6, "orientation": "front", "at": [18, 394, 18], "name": "Back", "material": "HDF 6"}
panel {"corner1": [0, 0], "corner2": [596, 746], "orientation": "front", "at": [2, -18, 2], "name": "Door", "material": "Birch ply 18"}
interfere {"create": false}
# → "pairs": []
cutlist {}
# → Left side and Right side 750 × 400 × 18, Bottom, Top and Shelf 564 × 394 × 18,
#   Back 714 × 564 × 6, Door 746 × 596 × 18
tool: inspect_drawing {"entities": false}
# → "counts": {"3D Solid": 7}: nothing else in model space
zoom {"mode": "extents"}
tool: render {"view": "iso"}
viewbase {}
rhinoout {"path": "C:/Users/me/Desktop/cabinet"}
# → cabinet.stp and cabinet.igs; open cabinet.stp in Rhino
```

## Table 1400 × 700 × 750 with a shaped top, a cable hole and four legs

```
rectang {"p1": [0, 0], "p2": [1400, 700], "fillet": 30}
# → <outline>
panel {"handle": "<outline>", "thickness": 25, "at": [0, 0, 725], "name": "Top", "material": "Oak 25"}
# → <top>
erase {"handles": ["<outline>"]}
# the outline was only a helper: PANEL keeps its profile
cylinder {"center": [1300, 600, 715], "radius": 30, "height": 45}
# → <cutter>: longer than the top is thick, so no faces coincide
subtract {"from": ["<top>"], "remove": ["<cutter>"]}
# → keeps <top>, erases the cutter
box {"corner1": [40, 40, 0], "corner2": [100, 100, 725]}
# → <leg1>
copy {"handles": ["<leg1>"], "delta": [1260, 0, 0]}
# → <leg2>
copy {"handles": ["<leg1>", "<leg2>"], "delta": [0, 560, 0]}
# → <leg3>, <leg4>
part {"handles": ["<leg1>", "<leg2>", "<leg3>", "<leg4>"], "name": "Leg", "material": "Oak"}
cutlist {}
# → Leg 725 × 60 × 60 quantity 4, Top 1400 × 700 × 25
interfere {"create": false}
# → "pairs": [] (the legs touch the top; touching doesn't count)
```

## Turned leg, 720 mm, revolved from a closed half-profile

The profile stands in the front plane with its axis edge on plane x = 0.

```
cplane {"preset": "front"}
pline {"vertices": [[0, 0], [25, 0], [25, 40], [18, 60], [22, 660], [30, 690], [30, 720], [0, 720]], "closed": true}
# → <profile>
cplane {"preset": "world"}
revolve {"handles": ["<profile>"], "axis": [[0, 0, 0], [0, 0, 1]]}
# → <leg>, one solid; the profile is erased
part {"handles": ["<leg>"], "name": "Turned leg", "material": "Beech"}
```

## Repairs

- **Loose surfaces after an extrude** (the lines or polyline are still there, or a
  `query_entities` record's mesh says "open: a surface, not a solid"):

  ```
  undo {"count": 1}
  join {"handles": ["<line1>", "<line2>", "<line3>", "<line4>"]}
  # → one closed polyline <profile>
  extrude {"handles": ["<profile>"], "height": 18}
  ```

- **A duplicate** (`interfere` reports two parts with the whole volume of one): erase the later
  handle with `erase {"handles": ["<copy>"]}`.
- **Interference solids** left by `interfere` without `"create": false`: its result's `handles`
  are those solids (layer INTERFERENCE); `erase` them.
