# Unreal Engine tools

`Plugins/TomBoxLevelImport` — editor plugin (UE 5.5) that imports an area of the second title's
map from the TomBox map server (`tools/rage/mapview/server.py`) as static meshes, textures,
material instances and a map. The plugin contains no game data; everything is read from the
user's own files through the local server, and the imported assets live in the user's UE
project (keep that project out of git).

Use:
1. Start the server: `python tools/rage/mapview/server.py <levels.rpf> <index dir> <ttpf_index.json>`.
2. Add the plugin to a UE 5.5 C++ project (`Plugins/TomBoxLevelImport`, a copy or a junction)
   and build the editor target.
3. In the editor: Tools > TomBox: Import Map Area... (centre in game metres, radius, detail
   level 0/1/2). Headless: `UnrealEditor-Cmd.exe <project> -run=TomBoxImport -center=X,Y
   -radius=M [-level=N] [-map=Name] [-notextures] [-nanite]`.

Coordinates: game metres, right-handed, become UE centimetres with Y mirrored; the area is
re-centred on the origin. Limits are those of the server (colour textures only, no LOD links,
no interiors; see docs/TECHNICAL_DEBT.md).
