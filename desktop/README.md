# Pow · PostgreSQL Query Studio

A local-first Tauri 2 + Solid.js desktop workflow for iterating on PostgreSQL queries. **Parsing is fully native Rust:** `src-tauri/src/native_plan.rs` uses a byte-oriented finite-state machine for PostgreSQL text EXPLAIN and `serde_json` for JSON EXPLAIN. It computes node metrics and stores the processed plan in Turso; Solid only displays the structured result. The existing PEV2 test fixtures are used as a compatibility reference; the Vue component library remains unchanged. Neither JavaScript plan parsing nor a regex engine is included in the shipped backend.

## Run

Requirements: Node.js 22+, Rust, [Tauri OS prerequisites](https://v2.tauri.app/start/prerequisites/) (WebKitGTK 4.1 on Linux) and a PostgreSQL server.

```sh
# from the repository root
cd desktop
npm ci
npm run tauri -- dev
```

`npm run build` builds the Solid frontend. `cargo test --manifest-path src-tauri/Cargo.toml` verifies the native Rust parser against PEV2 fixtures plus backend comparison, diagnostics, local Turso persistence, and CLI flags. The Rust binary uses `mimalloc` as its global allocator. Former JavaScript parser build files are preserved under `desktop/legacy/` for reference only; they are not built or shipped. `npm run tauri -- build` bundles the app. To build just the Debian package run `npm run tauri -- build --bundles deb`; it appears in `src-tauri/target/release/bundle/deb/`. GitHub Actions builds/tests/packages Linux, macOS and Windows under `.github/workflows/desktop.yml`.

## Launch from the terminal

The **v0.2+ `.deb` installs `/usr/bin/pow`**, so `pow` runs from a terminal after installing the Debian package. The old v0.1 package installed `/usr/bin/pow-desktop`; it does not provide the `pow` command. Without a `.deb`, with the build prerequisites installed, from `desktop/` run:

```sh
npm run cli:install          # Linux/macOS: builds and installs to ~/.local/bin/pow
pow                        # opens the desktop app
pow --help                 # usage
pow --version              # version
```

Ensure `~/.local/bin` is on your `PATH`. On Windows use `npm run cli:install:win` in PowerShell; it installs `pow.exe` under `%LOCALAPPDATA%\\Pow\\bin` and adds that directory to your user PATH (open a new terminal). Without installation, invoke `desktop/src-tauri/target/release/pow` (or `pow.exe`) directly after building. The executable embeds the UI and parser; Node.js is only needed to **build** it.

## Workflow

1. Create a workflow (pinned in the left sidebar).
2. Connect to PostgreSQL with host, port, username, database and password. Non-secret connection details are stored; **passwords are kept only in memory** and must be entered again after restarting. The current build uses unencrypted PostgreSQL connections: use a local server or a trusted tunnel, not an untrusted network.
3. Open query-window tabs. SQL drafts and titles auto-save to local Turso. Add a note before saving a step or explaining the query; each step freezes its SQL, note and optional **backend-parsed plan**. Previously saved raw plans are migrated by the backend on launch.
4. **Explain plan** does not execute the query. **Analyze** executes it inside a read-only, rolled-back transaction with a 15-second statement timeout and an explicit confirmation. PostgreSQL still performs reads and may call non-database side-effecting functions; only use Analyze with trusted SQL.
5. Click a step to inspect its saved plan in the **PEV2-style interactive diagram**. It uses PEV2's same `d3` zoom/pan and `d3-flextree` node layout; drag the background, use the wheel or zoom controls, click cards for node details, toggle duration/rows/cost and use Fit to recenter. Collapse the SQL editor to give the diagram room. Fork a tab to copy its draft, saved notes, EXPLAIN settings and history, or fork the whole workflow. Split view opens two independent tab selections side by side.
6. **Compare saved plans:** select a different step in the same query tab as the baseline. The backend reports changes in execution/planning time, estimated cost, estimated rows and operator counts. A missing execution time stays unavailable rather than being guessed; estimated row counts are shown neutrally rather than labeled improvements.
7. **Plan observations:** the backend highlights large sequential scans, disk-spilling sorts and large planned-vs-actual row mismatches. These are heuristics, not automatic tuning advice; always validate against your workload.
8. Choose EXPLAIN options per tab (Verbose, Buffers, WAL, Timing, Settings, Costs, Summary). Options are saved in Turso, with Analyze-only options applied only for `EXPLAIN ANALYZE`. Press **Ctrl+\\** (or **Cmd+\\** on macOS) or click **Note** to write a quick note, optionally attached to the selected step. Notes are saved immediately in Turso; Ctrl+Enter saves the dialog.
9. Open an existing shared Pow database by choosing **Database** in the sidebar and entering its file path. This switches workspaces without modifying the previously opened database; connection passwords must be re-entered. When sharing files, close Pow before copying its Turso database, including a WAL file if one exists. Do not use a network share without reliable file locking.
10. Choose a color scheme using **Theme** in the title bar. Gruvbox, Nord (default), Modus Operandi/Vivendi, One Dark, Kanagawa, Kanso, Rosé Pine, Solarized Light/Dark and GitHub Light/Dark are built in. The selected theme is saved locally in Turso and restored at launch. Creating or deleting workflows uses in-app dialogs (no browser prompts).

Local persistence defaults to Tauri's per-app data directory (`pow.turso`); no Turso cloud account is required. Large-log importing is not implemented yet: the sample log format and import semantics require confirmation before building the mmap pipeline. The UI cannot be used in a plain browser because data and PostgreSQL access go through Tauri commands. Deleting a workflow/tab permanently deletes its saved local history.
