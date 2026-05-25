# TongCraft BlueMap Renderer

Tauri desktop app for local, offline BlueMap render testing.

## Requirements

- Java 21+
- Minecraft Java world folder with `level.dat`
- `../bin/BlueMap-cli.jar`

## Development

```powershell
cd desktop-renderer
npm install
npm run tauri:dev
```

## Build

```powershell
cd desktop-renderer
npm run tauri:build
```

The app creates a separate render job directory and does not write into the selected Minecraft world.
