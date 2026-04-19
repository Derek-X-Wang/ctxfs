# ContextFS — Swift FSKit Appex

Vendored from [FSKitBridge](https://github.com/KhaosT/FSKitBridge) at commit 76e4b32.
Do not re-sync from upstream blindly — Phase 1.5 adds an auth handshake that
upstream does not have.

## Build

```bash
xcodebuild -project ContextFS.xcodeproj -scheme ContextFS -configuration Release
```

## Bundle IDs (locked)

- Host app: `ai.ctxfs.companion`
- Extension: `ai.ctxfs.companion.fskitext`

See `/docs/superpowers/specs/2026-04-11-fskit-backend-design.md` for architecture.
