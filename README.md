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

## Release

GitHub Actions publishes Windows release assets with `BlueMap-cli.jar` included:

- `*_setup.exe`: NSIS installer, recommended for normal users.
- `*-portable.zip`: portable package; extract the whole zip before running the app.

The app creates a separate render job directory and does not write into the selected Minecraft world.
