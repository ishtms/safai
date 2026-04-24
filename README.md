# safai

open-source system cleaner for mac, linux and windows. built on tauri + solid.

finds junk, duplicates, big old files, browser crud, startup hogs, and malware.
nothing gets hard-deleted, everything goes to a safai-owned graveyard so you can
restore the last clean if you change your mind.

## dev

```
pnpm install
pnpm tauri dev
```

needs rust (stable) and pnpm. on linux you also want webkit2gtk-4.1 and the
usual tauri deps.

## build

```
pnpm tauri build
```

spits out dmg + app on mac, msi + nsis on windows, appimage + deb on linux.

## tests

```
cargo test --lib       # rust
pnpm test              # frontend animation math
```

## layout

- `src/` solid frontend
- `src-tauri/src/` rust backend
  - `scanner/` per-feature modules (junk, dupes, treemap, largeold, privacy,
    startup, activity, malware)
  - `cleaner/` graveyard + audit + preview/commit/restore
  - `onboarding/` first-run flow + permissions
  - `scheduler/` cadence-based background scans
  - `volumes/` disk telemetry
