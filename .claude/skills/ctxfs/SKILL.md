---
name: ctxfs
description: Mount third-party library source code as local directories so you can read, grep, and navigate it with normal file tools. Use this whenever you need to understand how a library, framework, SDK, or package works — checking APIs, debugging errors coming from a dependency, finding usage examples in real code, verifying what a function actually does, or tracing behavior through library internals. Prefer this over web search when reading the source would answer the question. Works with npm, PyPI, crates.io, and GitHub repos.
---

# ctxfs — Mount Dependency Source for Reading

## What This Skill Does

ctxfs is an AI-native read-only filesystem that mounts remote source code as local directories without cloning. Files are fetched lazily on access. Once mounted, you can use `Read`, `Grep`, and `Glob` on the mount point as if the code lived locally.

Use this skill when you need real source code for a dependency. When ctxfs isn't usable (see Step 0), fall back to training knowledge rather than spinning.

## When To Use

Trigger when any of these apply:

- **Understanding a library**: "How does React's `useEffect` actually work?" → mount React, read the source
- **Debugging a dependency error**: A stack trace points into `node_modules/lodash/get.js` → mount lodash, read the function
- **Finding API usage**: "Does `requests.Session` have a `close()` method?" → mount requests, grep for `def close`
- **Verifying behavior**: "Is tarpc's `context` Send+Sync?" → mount tarpc, check the trait definitions
- **Tracing behavior**: "What does `serde_json::from_str` do when input is empty?" → mount serde_json, follow the code

Don't use this skill for code the user already has locally (use `Read`/`Grep` directly on their files).

---

## macOS First-Time Setup

**On macOS 26+ (Tahoe), the FSKit backend is available and recommended.** It requires no sudo and no Full Disk Access. Run `ctxfs setup install-fskit` to enable it, then skip steps 1 and 2 below.

Two one-time steps before ctxfs will work on macOS (required if not using FSKit):

1. **Passwordless sudo for NFS mounts**: run `ctxfs setup install` from your terminal (prompts for your password once, then never again).

2. **Full Disk Access for your terminal app**: macOS treats NFS mounts (even loopback ones) as network volumes and blocks access unless your terminal has Full Disk Access. Without this, mounts succeed but reads fail with "Operation not permitted". To fix, open this pane and grant access to your terminal app (Terminal, iTerm2, cmux, Ghostty, etc.), then restart it:

```bash
open "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles"
```

This is the same restriction that affects macFUSE, s3fs-fuse, and HuggingFace's hf-mount — see [macfuse#690](https://github.com/macfuse/macfuse/issues/690). It's not a ctxfs bug.

**Linux** only needs step 1. No FDA equivalent exists.

---

## Step 0: Feasibility Check (do this first, always)

Before touching ctxfs, confirm the environment can actually use it. This is the single biggest failure mode.

Run these checks in order. Stop at the first one that fails and take the fallback action.

### Check 0a: Can you run Bash commands?

ctxfs is a CLI — without Bash, nothing works. If your session is read-only or Bash is denied:

> **Fallback**: You can't run ctxfs here. Tell the user: "I can't run ctxfs in this session (Bash is not available). I'll answer from training knowledge, but for the most accurate source-grounded answer, please run the ctxfs mount yourself and then re-ask." Then answer using your existing knowledge of the library. Be honest about the source: cite training knowledge, not fake file paths.

### Check 0b: Is the daemon running? Start it if not.

```bash
ctxfs daemon status
```

- "Daemon is running (pong)" → continue to 0c
- "Daemon is not running" → start it: `nohup ctxfs daemon start > /tmp/ctxfs-daemon.log 2>&1 &` then wait ~500ms and re-check status
- "Daemon unreachable" / other errors → surface the error to the user, ask how to proceed

If `ctxfs` isn't on `PATH`, try `ctxfs-dev` (a symlink to the local dev build). If neither exists, substitute the build path (e.g., `./target/release/ctxfs`). If the binary doesn't exist at all, ask the user to build or install it first.

### Check 0c: Is passwordless sudo configured for mounts?

NFS kernel mounts require sudo on macOS and Linux. Without passwordless sudo, the mount command will prompt for a password — which hangs forever in non-interactive Bash.

```bash
ctxfs setup check
```

- "Configured: /etc/sudoers.d/ctxfs exists" → continue to Step 1
- "Not configured" → **stop and escalate** (see fallback below)

> **Fallback for "Not configured"**: Tell the user: "ctxfs needs passwordless sudo to run NFS kernel mounts non-interactively. Please run `ctxfs setup install` **from your own terminal** (not through me — the install itself prompts for a sudo password and needs a real TTY). Alternatively, I can print the exact `sudo mount_nfs` command for you to run manually one time. Which do you prefer?" Wait for the answer. Do NOT try to run `ctxfs setup install` or `ctxfs mount` yourself — both will hang.

### Check 0c-fskit: Is the FSKit backend available and preferred?

On macOS 26+, ctxfs supports a FSKit backend that does not require sudo or Full Disk Access.

```bash
ctxfs setup check
```

Look at the "FSKit backend" section of the output:

- **macOS version: 26+** AND **App installed: yes** → FSKit is ready. `ctxfs mount` will use it automatically (or you can force it with `--backend fskit`). Skip checks 0c and 0d entirely — they apply only to the NFS backend.
- **macOS version: <26** → FSKit not available on this system. Continue with check 0c.
- **App installed: no** → FSKit available but not yet set up. Run `ctxfs setup install-fskit` to enable it. After install, FSKit is ready and you can skip 0c and 0d.

### Check 0d: Can you actually read files from an NFS mount?

On macOS, the process running your session needs **Full Disk Access** to read files from NFS mounts. Without it, the mount succeeds but every `Read`, `Grep`, or `ls` against the mount point returns `EPERM / Operation not permitted` — the kernel blocks the syscall at the client side before it ever reaches ctxfs's NFS server.

This is macOS TCC enforcement treating NFS mounts (including loopback ones) as "network volumes". It affects ctxfs the same way it affects macFUSE, s3fs-fuse, and other user-space filesystem tools — see [macfuse#690](https://github.com/macfuse/macfuse/issues/690) for the canonical discussion. HuggingFace's `hf-mount` uses the same NFS-loopback approach as ctxfs and hits the same issue.

The cheapest way to detect it is empirical: after the first mount, attempt to read a known file. If the read fails with `EPERM` despite `mount | grep ctxfs` showing the filesystem is live, the runtime is missing Full Disk Access.

> **Fallback when Full Disk Access is missing**: Tell the user:
>
> "I can mount sources successfully, but my session can't read NFS volumes because macOS's TCC is blocking the access. To fix this permanently:
>
> 1. Identify the process running this session — usually your terminal app (Terminal.app, iTerm2, Ghostty), Claude Code itself, or a containerized tool like cmux.
> 2. Open **System Settings → Privacy & Security → Full Disk Access**.
> 3. Toggle that app ON (click `+` to add it if it's not listed).
> 4. **Restart the session** — TCC grants only apply to new processes.
>
> Until then I'll fall back to training knowledge for this question."
>
> Then answer from training knowledge.
>
> **Note on stuck mounts**: If you created an NFS mount and now the access is blocked, the mount can only be cleaned up from a terminal that DOES have Full Disk Access (e.g., your own terminal app running `sudo umount /path/to/mount`). The ctxfs daemon itself can't force-unmount a sandboxed mount.

### Check 0e: Is GITHUB_TOKEN set?

```bash
echo "${GITHUB_TOKEN:+set}"
```

- `set` → good, you have 5000 req/hr
- empty → you have 60 req/hr unauthenticated. Proceed, but if the user asks for many libraries, warn them about the limit and suggest setting `GITHUB_TOKEN`.

---

## Step 1: Decide what to mount

**If the user's project has a manifest** (`package.json`, `Cargo.toml`, `requirements.txt`, `pyproject.toml`), prefer `ctxfs deps`:

```bash
ctxfs deps list .                                          # see what's available
ctxfs deps mount . --select lodash -d ./ctxfs-deps          # one package, non-interactive
ctxfs deps mount . --select react,lodash,axios -d ./ctxfs-deps  # several
ctxfs deps mount . --all -d ./ctxfs-deps                    # everything (be careful)
```

**If the library isn't in the manifest** (transitive dep, or outside the project), use a direct source spec:

```bash
ctxfs mount npm:react@19.1.0 -p ./ctxfs-deps/react
ctxfs mount pypi:requests@2.31.0 -p ./ctxfs-deps/requests
ctxfs mount crate:serde@1.0.219 -p ./ctxfs-deps/serde
ctxfs mount github:tokio-rs/tokio@tokio-1.40.0 -p ./ctxfs-deps/tokio
ctxfs mount npm:react@19.1.0 crate:serde@1.0.219 -d ./ctxfs-deps  # multi-mount
```

**Source spec format**: `<provider>:<name>@<version>[:subpath]`
- providers: `github`, `npm`, `pypi`, `crate`
- GitHub uses `owner/repo@ref` (branch, tag, or commit SHA)
- Registry specs auto-resolve to the GitHub repo where the source lives

## Step 2: Read and grep the mounted source

```
Read    → /path/to/ctxfs-deps/react/packages/react/src/ReactHooks.js
Grep -n → "function useEffect" ./ctxfs-deps/react/
Glob    → ./ctxfs-deps/react/**/*.ts
```

First access to a file triggers a lazy GitHub fetch. Subsequent accesses hit the local blob cache.

## Step 3: Answer the user's question

Ground your answer in actual file paths and code snippets. Avoid the temptation to pad with generic explanation once you have the real source. Specificity is the whole point of doing this.

## Step 4: Clean up

```bash
ctxfs unmount ./ctxfs-deps/react        # single mount
ctxfs deps unmount -d ./ctxfs-deps      # everything under a directory
ctxfs unmount --all                     # all active mounts (regardless of location)
```

If the conversation is ongoing and more deps might come up, leave mounts in place — they're cheap when idle.

---

## Common Patterns

**Pattern 1: "How does X work?"**

```bash
ctxfs daemon status                                      # Step 0b
ctxfs setup check                                        # Step 0c
ctxfs mount crate:tokio@1.40.0 -p ./ctxfs-deps/tokio    # Step 1
# Grep for the definition, Read the file, explain
ctxfs unmount ./ctxfs-deps/tokio                         # Step 4
```

**Pattern 2: Debugging a dependency error**

Stack trace mentions `node_modules/axios/lib/core/InterceptorManager.js:42`:

```bash
ctxfs deps mount . --select axios -d ./ctxfs-deps
# Read ./ctxfs-deps/axios/lib/core/InterceptorManager.js and look at line 42
```

**Pattern 3: Multi-dep exploration**

```bash
ctxfs deps mount . --select react,react-dom,react-router -d ./ctxfs-deps
# All three mounted in parallel under ./ctxfs-deps/
```

**Pattern 4: Agent / non-interactive use**

Always use explicit flags, never the interactive picker:

```bash
ctxfs deps mount . --all --include-dev -d ./ctxfs-deps  # good
ctxfs deps mount .                                       # bad — will try to open a TTY picker
```

---

## What About --server-only?

`--server-only` starts the NFS server without kernel-mounting anything. **It is NOT a workaround for the sudo requirement.** Without a kernel mount, there is no directory you can `Read` from. Use it only when you want to verify the daemon side of the flow works (e.g., resolving a ref, fetching the tree) while deliberately skipping the mount step.

If you can't do a kernel mount, the right path is to stop and ask the user to help — not use `--server-only` as a fallback.

---

## Troubleshooting

**"failed to connect to daemon"** — The daemon isn't running. See Step 0b.

**"rate limited: retry after Ns"** — GitHub API limit. Set `GITHUB_TOKEN` in the daemon's environment and restart it. Until then, fall back to training knowledge for well-known libraries.

**"already mounted"** — The mount point is in use. Either use a different `-p` path, read from the existing mount, or unmount first.

**"mount command exited with status: exit status: 1"** — Usually a sudo failure. See Step 0c.

**`ctxfs setup check` says "Not configured" but sudoers is actually installed** — Known issue: `setup check` can report false negatives. Verify directly: `ls /etc/sudoers.d/ctxfs` (should exist) and `sudo -n mount_nfs` (should return usage without prompting for a password). If both pass, setup is actually configured and you can proceed.

**"Operation not permitted" when reading a mounted file** — macOS TCC is blocking NFS access. See Step 0d. This is NOT a file-permission issue; the mount is world-readable but the runtime process lacks Full Disk Access. The fix is confirmed working on macOS Tahoe 26 with cmux: grant Full Disk Access to the session's host app in System Settings → Privacy & Security → Full Disk Access, then restart the session. Do NOT spin on this — fall back to training knowledge and ask the user to apply the grant.

**"sudo: a password is required"** — Passwordless sudo is not configured. See Step 0c fallback.

**"no GitHub repository found"** — The package on npm/PyPI/crates.io doesn't list a GitHub source URL. Find the repo manually and use the `github:owner/repo@ref` form.

**Mount succeeds but files look empty** — First-access latency for the NFS cache. Retry the read once. If it keeps failing, unmount and remount.

**Spinning on a denied tool** — If you've tried the ctxfs command twice and Bash/Read/Write keep getting denied, stop. You're in a sandboxed environment. Fall back to training knowledge and tell the user you couldn't access the source directly.

---

## Why Read Source Instead Of Guessing?

- **Accuracy**: Source code is the ground truth. Docs and web answers can be stale, wrong, or for the wrong version.
- **Specificity**: You can check the exact version the user has installed. "React 19.0.0 does X" beats "React generally does X".
- **Completeness**: Grep for all call sites, all implementations, all related functions — things web search can't easily give you.
- **Speed**: After the first fetch, everything is cached locally.

When the answer lives in code, read the code. When the environment won't let you read the code, be honest about it.
