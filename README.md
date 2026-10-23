# PortageHub

A Steam-styled graphical front end for Gentoo's Portage, written in Rust (egui/eframe).
Everything you would do with `emerge`, `eselect repository` and the files in
`/etc/portage` is available as buttons, switches and menus — with plain-language
explanations for newcomers and every expert option still within reach.

## Features

- **Store** — browse all packages from every repository, category tiles, fast search
  (name or description, per-repository filter), possible updates, unread news.
- **Library** — installed packages Steam-library style: filter by @world, dependencies,
  updates or orphans (installed but no longer in any repository).
- **Package page** — big INSTALL / UPDATE / REBUILD button with an options menu
  (specific version, slot, `--oneshot`, fetch only, binary packages, depclean, unmerge,
  @world select/deselect, `pkg_config`), and tabs for:
  - *Versions* grouped by slot with STABLE / TESTING / NO KEYWORD / LIVE / MASKED badges
    (including the reason from the repository's `package.mask`); click a version for
    install, keyword, mask/unmask and license actions.
  - *USE flags* with descriptions (`metadata.xml`, `use.desc`, `use.local.desc`,
    USE_EXPAND), a Default/On/Off switch per flag, scope (all versions / slot / version)
    and “ask emerge for the effective flags”.
  - *Dependencies* as a tree with USE conditionals, installed markers, click-through,
    and reverse dependencies.
  - *Ebuild* source with syntax highlighting, `files/` list; *Files* (CONTENTS);
    *History* (build times from `emerge.log`); *My settings* (every `/etc/portage`
    line affecting the package, with on/off switches).
- **Emerge** — one-click recipes (update @world, apply USE changes, preserved-rebuild,
  depclean, fetch only, resume, empty tree), and a builder that exposes every emerge
  option as a switch / yes-no-default / number field with help text and the equivalent
  command line. Every change runs as a **preview** first (`--pretend`), parsed into:
  - the merge list (NEW / UPDATE / REBUILD / DOWNGRADE / NEW SLOT, binaries, changed USE),
    estimated build time from your own history and download size;
  - **conflict handling**: autounmask suggestions as checkboxes you can apply in one
    click (then the preview re-runs), masked packages with “Allow this version”, slot
    conflicts with retry options (backtracking, full world update, verbose conflicts,
    autounmask backtracking), blockers, REQUIRED_USE, circular dependencies with USE
    fixes, ambiguous names, missing packages (search / find an overlay), news,
    preserved libraries and config-file updates.
- **Activity** — live progress of running emerges (also for parallel `--jobs`): per
  package stage (queued, building with the current phase from the build log,
  installing, done, failed), elapsed vs. expected time, overall progress and ETA,
  filterable coloured log, Stop / Resume / Resume-skip-first. A Steam-style
  “EMERGING” bar stays visible in the footer on every page.
- **Overlays** — repository cards (sync type, priority, ebuild count, last sync, masters,
  file), per-repo sync, `egencache`, inline repos.conf editing, remove/disable, the
  official overlay list (`eselect repository`), and custom git/rsync/hg/svn or local
  overlays.
- **Configuration** — every `package.*` file (`use`, `accept_keywords`, `mask`,
  `unmask`, `license`, `env`, `properties`, `accept_restrict`, legacy `keywords`,
  sets, profile overrides) in file or directory form: lines become rows with an
  on/off switch (comment/uncomment), edit, delete, a guided “add entry” wizard and a raw
  editor. Plus make.conf cards (MAKEOPTS sliders, branch, licenses, FEATURES chips,
  global USE, VIDEO_CARDS/INPUT_DEVICES, all variables with effective values),
  `/etc/portage/env` files with templates, profile switching, news, `._cfg` config
  updates with diffs, and a **stray-file detector** for files Portage silently ignores
  (e.g. `package.accept_keywordschatgpt-desktop`), which it can merge into the right place.
- **History** — every merge, failure, removal and emerge command from `emerge.log`,
  compile-time totals and a per-day chart.

Every write to `/etc/portage` is shown as a diff first (can be turned off) and written
atomically; new entries go to a dedicated `zz-portagehub` file in each directory.

## Build and run

```sh
cargo build --release
./target/release/portagehub
```

Keep `portagehub` and `portagehub-helper` next to each other (or install them, below).
Your user should be in the `portage` group so the history and live build logs are readable.

### Administrator rights

Browsing and previews run as your user. The first action that needs root starts
`portagehub-helper` through **pkexec** (polkit asks for your password once per session).
The helper only runs Portage tools (`emerge`, `emaint`, `eselect`, `egencache`, …),
writes only below `/etc/portage`, and otherwise only accepts or discards `._cfg` config
updates below `/etc`. If you close PortageHub during an emerge, the helper keeps it
running until it finishes.

To install system-wide (adds the polkit policy so the prompt shows a proper message and
is remembered briefly):

```sh
cargo build --release
sudo dist/install.sh
```

### Environment variables

| Variable | Purpose |
| --- | --- |
| `PORTAGEHUB_ELEVATE` | Launcher for the helper: `pkexec` (default), another command such as `doas`, or `none` to run it unprivileged (testing). |
| `PORTAGEHUB_DEMO=1` | Replays a synthetic parallel emerge to try the progress view without building anything. |
| `PORTAGEHUB_SCRIPT` | UI automation for screenshots, e.g. `tab=library;open=app-editors/vim;dtab=use;wait=2;shot=/tmp/vim.png;exit`. |

`portagehub --self-test` prints what was read from your system (repositories, tree,
installed packages, history, masks, configuration, update estimate) without opening a window.

## Tests

```sh
cargo test
```

Unit tests cover PMS version ordering, atoms, dependency strings, repos.conf,
make.conf, `package.*` parsing and editing, the pretend-output parser, progress tracking
and emerge.log parsing; the integration test drives the real helper binary over its
protocol.
