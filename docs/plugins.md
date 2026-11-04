# Lua plugins

Plugins add actions on files and folders, forms that ask for options first, background jobs (including FFmpeg conversions), a bar under the file list, inspector sections and event hooks. They are written in Lua 5.4 and run in a sandbox: what a plugin may touch is declared in its manifest and approved by the user.

The implementation is in `src/plugins/`; the bundled plugins are in `plugins/` and are good examples.

## Layout

```
<config dir>/plugins/                Settings → Plugins shows the exact path
  .luarc.json                         written by DirFleet, for the Lua language server
  types/dirfleet.d.lua                the API's type definitions (autocomplete)
  my.name.thumbnails/                 one folder per plugin
    plugin.toml                       the manifest
    main.lua                          returns the plugin's table
    lib/*.lua                         optional, loaded with require("lib.name")
```

Set `DIRFLEET_PLUGINS_DIR` to use another folder. **Settings → Plugins** opens the folder, lists plugins with their status, permissions, shortcuts and recent errors. Saving a file reloads that plugin; if the new version fails to load, the last working version keeps running and the error is shown.

A user plugin with the same `id` as a bundled one replaces it, which is the way to customise the bundled plugins: copy the folder from `plugins/` and edit it.

## The manifest

```toml
api_version = 1                       # required; only 1 exists
id = "my.name.example"                # reverse-DNS: lowercase, digits, '-', at least two parts
name = "Example"
version = "1.0.0"                     # MAJOR.MINOR.PATCH
author = "Me"
description = "What it does."
permissions = ["fs.read", "process:git"]
max_jobs = 1                          # this plugin's jobs running at once (default 1)

[activation]                          # where the bar, folder actions and inspector apply
ancestor_contains = [".git"]          # also: contains_any = [...], glob = "*.sln", always = true

[[action]]
id = "convert"                        # full id: my.name.example.convert
title = "Convert…"
icon = "film"                         # a lucide icon name
shortcut = "Ctrl+Alt+M"               # default; users can change it; Ctrl means Cmd on macOS
palette = true                        # in the command palette (default true)
context_menu = "entry"                # none | entry (selected items) | background (the folder) | both
targets = { kind = "files", ext = ["mov", "avi"], min = 1, max = 50, glob = "*.mov" }
form = "dynamic"                      # none | dynamic (Lua builds it) | static ([[action.field]])
mode = "job"                          # quick (2 s budget) | job (own thread, progress, cancel)
per_item = true                       # call run_item once per target (jobs only)
confirm = "Really?"                   # ask before running (when there is no form)
results = true                        # a dialog with the options and results; see Tables (needs mode = "job")

[bar]
handler = "bar"                       # M.bar(ctx) returns the segments
refresh_secs = 30                     # also refreshed when the folder changes

[inspector]
title = "Media"
handler = "inspector"
targets = { kind = "files", max = 1 }

[events]
subscribe = ["folder_opened", "selection_changed", "fs_changed", "file_op_finished", "job_finished", "app_started"]
```

Unknown keys are errors, reported with their line.

`targets.kind` is `any` (the default), `files`, `dirs` or `current_dir`. A `current_dir` action acts on the folder shown, ignores the selection, and appears only where the activation matches. `min` defaults to 1.

### Permissions

| Permission | Allows |
| --- | --- |
| `fs.read` | `dirfleet.fs` reading, `app.system_open` |
| `fs.write` | `fs.write_text`, `fs.mkdir`, `dirfleet.ops.*` |
| `process:<name>` | `process.run` of that program, e.g. `process:git` |
| `ffmpeg` | `dirfleet.ffmpeg.*`, `dirfleet.media.probe` |
| `clipboard` | `clipboard.set_text` |

Bundled plugins are trusted. A user plugin that asks for permissions stays off until the user clicks **Allow** in Settings. If an update asks for more, it needs approval again.

## main.lua

`main.lua` returns a table, `M`:

```lua
local M = { actions = {} }

-- A quick action: a function is its `run`.
function M.actions.copy_names(ctx)
  local names = {}
  for _, t in ipairs(ctx.targets) do names[#names + 1] = t.name end
  dirfleet.clipboard.set_text(table.concat(names, "\n"))
end

-- Or a table with form, validate, run / run_item.
M.actions.convert = {
  form = function(ctx) return { fields = { ... } } end,
  validate = function(ctx, values) end,          -- nil, or { field_id = "message" }
  run_item = function(ctx, item, values) end,    -- per_item jobs
}

function M.bar(ctx) return { { text = "hello" } } end
function M.inspector(ctx) return { { "Label", "value" } } end
function M.on_folder_opened(ctx, data) end
return M
```

Every handler gets `ctx` with these fields:

| Field | Contents |
| --- | --- |
| `dir` | The folder shown. |
| `root` | The folder that matched `ancestor_contains`, e.g. the repository root. |
| `targets` | A list of `{ path, name, stem, ext, is_dir, size, modified }`. |
| `values` | The submitted form values. |
| `tab_id` | The tab the handler runs for. |
| `plugin` | `{ id, name, version }` |

What a job's `run` or `run_item` returns sets how the item is reported:

| Return value | Reported as |
| --- | --- |
| nothing or `true` | done |
| `false`, `"skipped…"` or `{ skipped = true }` | skipped |
| `{ error = "why" }` | failed (as does raising an error) |

When a job ends, its summary ("3 done, 1 skipped") is shown, and the status bar's plugin activity list keeps it.

## Forms

Forms are data. Lua builds them once, the app draws them, and no Lua runs while the dialog is open, which keeps it responsive. A dynamic form can look at the targets first, for example probing a video for its length:

```lua
form = function(ctx)
  local info = dirfleet.media.probe(ctx.targets[1].path)
  return {
    title = "Convert " .. ctx.targets[1].name,
    submit = "Convert",
    remember = true,              -- start from the last values next time
    fields = {
      { type = "info", text = info and dirfleet.fmt.duration(info.duration) },
      { id = "format", type = "choice", label = "Format", options = { { "mp4", "MP4" }, { "webm", "WebM" } }, default = "mp4" },
      { id = "range", type = "radio", options = { { "all", "Whole" }, { "part", "Fragment" } }, default = "all" },
      { id = "start", type = "time", label = "From", max = info and info.duration,
        visible_if = { field = "range", equals = "part" } },
    },
  }
end
```

A form can also set:

- `danger = true`: the submit button is red, for what can't be undone;
- `submit_when = { { when = { field = "x", equals = v }, label = "Delete", danger = true } }`: another look for the submit button while a condition holds;
- `blocked = true`: there is nothing to submit, as its fields explain, so only **Close** is offered.

Field types:

| Type | Notes |
| --- | --- |
| `info` | Read-only text, from `text`, across the whole dialog. |
| `paths` | What an action will affect, from `rows = { { path = "...", size = "1 GB" } }`, in a card. |
| `separator` | A divider. |
| `group` | A heading (`label`) over its own `fields`. |
| `text`, `multiline` | `placeholder`; `required` means not blank. |
| `int`, `float` | `min`, `max`, `step`; `slider = true` draws a slider. |
| `bool` | A checkbox. |
| `choice`, `radio` | `options` as `{ "value", "Label" }`, `{ value =, label = }` or `"value"`. |
| `time` | Seconds, typed as `h:mm:ss.mmm`, with `min` and `max`. |
| `path` | A text field. |

Every field can have:

- `id`, `label`, `default` and `help` (a tooltip);
- `enabled = false`;
- `visible_if` and `enabled_if`: `{ field = "x", equals = v }`, or with `not_equals`, `in = {…}` or `not_in = {…}` in place of `equals`.

The app checks values (types, ranges, required) before calling `validate`. A static form lists the same fields in the manifest under `[[action.field]]`.

## The API

The full list with types is in `plugins/types/dirfleet.d.lua`. In short:

| Table | Functions |
| --- | --- |
| `dirfleet.path` | `join`, `parent`, `name`, `stem`, `ext`, `with_ext`, `is_absolute`, `unique` ("name (2).ext"), `home` |
| `dirfleet.fmt` | `duration(seconds)`, `size(bytes)` |
| `dirfleet.fs` | `exists`, `is_dir`, `is_file`, `stat`, `read_dir`, `read_text`, `glob`, `usage`, `usage_many`, `write_text`, `mkdir` (absolute paths only) |
| `dirfleet.ops` | `copy`, `move`, `trash`, `delete`, `rename`: through the app's file operations, with progress and, except for `delete`, undo |
| `dirfleet.process` | `run{ cmd, args, cwd, env, timeout, stdin, on_line }` returns `{ code, ok, stdout, stderr }` |
| `dirfleet.ffmpeg` | `run{ args, duration, on_progress }`, `available`, `version`, `encoders` |
| `dirfleet.media` | `probe(path)` returns `{ duration, width, height, fps, video_codec, audio_codec, has_audio, … }` |
| `dirfleet.app` | `open`, `open_in_new_tab`, `refresh`, `select`, `system_open`, `open_terminal`, `show_table`, `update_table_row` |
| `dirfleet.job` | `progress(fraction, label)`, `cancelled()` |
| `dirfleet.storage` | `get`, `set`: a JSON file per plugin |
| `dirfleet.bar`, `dirfleet.inspector` | `invalidate()` |
| other | `dirfleet.notify(text, level)`, `dirfleet.log.info/warn/error`, `dirfleet.clipboard.set_text` |

How errors come back:

- A missing permission raises an error that names it.
- A failed operation returns `nil, message`.
- `print` writes to the log.

### Folder sizes

`dirfleet.fs.usage(path, { newest_skip = { "target" } })` returns `{ bytes, freed, files, newest, errors, parts }`. It counts allocated disk space with hardlinks counted once, doesn't follow symlinks and stays on one filesystem, so its result matches `du`. Adding up `stat` sizes reads about twice the truth on a Cargo `target` folder, where outputs are hardlinked. `freed` is what deleting the folder would free: `bytes`, less files that also have links outside it, which is about 40% of a Cargo `incremental` folder. `newest` is the newest file's time, leaving out folders with the names in `newest_skip`. On Windows `bytes` adds up file sizes, so hardlinks count more than once.

`subtotals = { "target" }` also fills `parts.target` from the same walk, for folders directly under the root. That's much faster than walking `target` again. On macOS file details are read in batches, which takes about 2.5 times less on large folders.

`dirfleet.fs.usage_many{ paths, newest_skip, subtotals, on_result }` walks up to 8 folders at once on threads of its own. `on_result(path, usage, error)` runs on the plugin's thread as each folder finishes, in no set order. It returns `true`, or `nil, "cancelled"`.

### Tables

An action with `results = true` opens a dialog holding its options and results, instead of a form that closes when it runs. Its form must be `static` or `none`, and its `mode` must be `job`. The job fills in the results with `dirfleet.app.show_table`, as often as it likes, and each call replaces the previous one:

```lua
dirfleet.app.show_table({
  title = "Largest folders",
  subtitle = "3 folders",
  progress = "Scanning 1 of 3",                         -- leave it out of the last call
  columns = {
    { id = "name", label = "Folder" },
    { id = "size", label = "Size", kind = "size" },     -- bytes
    { id = "when", label = "Changed", kind = "time" },  -- seconds since 1970
  },
  rows = { { path = "/a/b", cells = { name = "b", size = 1024, when = 1700000000 } } },
  sort = { column = "size", descending = true },
})
```

The dialog does the rest:

- Radio fields become segmented buttons above the table, beside a **Hide smaller than** control when there's a `size` column.
- **Run** becomes **Restart** while a job runs, next to **Stop**. A restart uses the current options and ignores anything the replaced job still sends.
- **Run in background**, **Close**, Esc or clicking outside hide the dialog and leave the job running. The status bar shows an item that brings it back, and so does running the action again.
- If the job ends without a last call, rows stay and it says it stopped early. Results last until the app quits or the status bar's × discards them.
- Running the action from another folder while idle keeps the old results, labelled, until **Run**.

The size filter applies to the first `size` column straight away with no new run, and stays for the next run. Rows without a size yet stay visible.

Clicking a header sorts by that column and clicking again reverses it. The sort stays across updates. Ties keep the previous order, so sorting by date and then by size means size, with ties by date. A missing cell is blank and sorts last. Double-clicking a row with a `path` hides the dialog and opens that folder in a new tab.

A subtitle can use `{count}` and `{total}`, filled in from the rows now in the table.

Right-clicking a row with a `path` offers **Open in new tab** and **Copy path**. A row's `menu` adds entries that start an action on its folder, in this plugin or another:

```lua
{ path = "/a/b", cells = { ... }, menu = {
  { label = "Remove folder…", action = "my.name.example.remove", danger = true,
    busy = "Removing…", title = "Remove b?" },
} }
```

The action's form is the confirmation, so it should say exactly what will go, with a `paths` field. Cancelling or confirming returns to the results. `title` is the confirmation's title, shown while the plugin prepares it. These entries are disabled while the table's own job runs, since it may be reading that folder.

From confirming until new sizes arrive, the row shows what is happening beside its name and its entries are disabled: **Waiting…** behind another job, then the entry's `busy` text, then **Measuring…**. If the action fails, the row says so with the reason in its tooltip and keeps its sizes.

Set `refresh_action` on the table to an action of this plugin that measures one folder again. It is run with `ctx.dir` set to the row's folder when a menu entry's job has ended and the file operations plugins asked for have finished, since `dirfleet.ops` only asks for the deleting to start. It also adds **Measure again** to the menu. The action reports with `dirfleet.app.update_table_row{ path, cells, clear, remove }`.

Calling `show_table` or `update_table_row` from a plugin with no `results = true` action is an error.

### FFmpeg

`dirfleet.ffmpeg.run` uses the FFmpeg the app uses for video previews: next to the executable, else the one it downloaded, else `PATH`. When it's missing, the download starts, and jobs wait up to three minutes for it.

The app adds `-hide_banner -nostdin -loglevel error -progress pipe:1`. With `duration` (seconds) given, FFmpeg's progress drives the job's progress bar. Cancel kills FFmpeg and anything it started.

## Limits and threading

- **Threads.** Each enabled plugin has a worker thread with its own Lua state. Forms, validation, quick actions, the bar, the inspector and events run there, one at a time. A newer bar or inspector request replaces one that is still waiting.
- **Quick calls.** Lua code gets 2 seconds of running time. Waiting on a process, a file or FFmpeg doesn't count. Processes started from a quick call time out after 30 seconds unless they set `timeout`.
- **Jobs.** Each job runs on its own thread with a fresh Lua state, with no time limit. At most 2 jobs run at once across all plugins, and `max_jobs` per plugin. Job code shouldn't rely on globals set by the worker; use `dirfleet.storage`.
- **Memory.** Each Lua state is limited to 64 MB.
- **Removed from Lua.** `io`, `os` (except `os.time`, `os.clock` and `os.date`, which is UTC), `package`, `debug`, `dofile`, `loadfile` and `string.dump` are gone. `load` takes text only. `require` loads only the plugin's own files.

## Bundled plugins

| Plugin | What it does |
| --- | --- |
| Git (`plugins/io.github.leinnan.git`) | Inside a repository: branch, ahead/behind and changes in the bar, Fetch, Pull and Push jobs, Commit… with a message form, and the selected file's status and last commit in the inspector. |
| Video Converter (`plugins/io.github.leinnan.video-convert`) | **Convert to MP4** on `.mov` and `.avi` writes `name.mp4` next to each file, adding " (2)" when it exists. **Convert video…** asks for format (MP4, MKV, WebM, GIF, MP3), quality, height, whole video or a fragment, audio, and what to do when the file exists. The inspector shows a video's length, resolution, codecs and bit rate. |
| HEIC Converter (`plugins/io.github.leinnan.heic-convert`) | **Convert to JPEG** on `.heic` and `.heif` writes `name.jpg` next to each file, adding " (2)" when it exists. **Convert photo…** asks for format (JPEG, PNG, TIFF), JPEG quality, longest side (photos are never enlarged), and what to do when the file exists. macOS uses `sips`, which keeps the capture date, location and other metadata. Windows and Linux use FFmpeg 8.1 or newer: JPEG and TIFF lose that metadata, PNG keeps it. |
| Folder Usage (`plugins/io.github.leinnan.folder-usage`) | **Folder Usage…** scans the current folder in a job and shows a table of its subfolders, or of the Rust projects anywhere below it. Options and results share one dialog, which fills in as each folder finishes, can hide folders below a size, offers **Clear incremental…**, **Clear target…** and **Remove folder…** from a right-click on a row, each with a confirmation listing what goes (Remove folder moves to the Trash unless told otherwise, and refuses the disk root and folders holding your home), can stop or restart with other options, and can be hidden while it runs: the status bar brings it back. Size is real disk use and **target** is the part of it that is Cargo's output. **Last change** is the newest file outside `target`, `.git` and `node_modules`; a fresh clone or checkout resets it. |
| Rust Clean (`plugins/io.github.leinnan.rust-clean`) | Inside a Rust project: **Rust: Clear target…** runs `cargo clean`, and **Rust: Clear incremental…** deletes only the `incremental` caches, which Cargo has no flag for. Both show exact paths and sizes first and delete permanently. Paths come from `cargo metadata`, so workspace members clear the workspace's folder. A target folder outside the project is refused, since other projects may share it. When Cargo can't read a project, `<project>/target` is used, but only if it has `CACHEDIR.TAG` or `.rustc_info.json`. Clearing incremental caches during a build can make that build fail; run it again. |

## Tests

```text
cargo test --offline plugins
cargo test --offline plugins::tests::real_ffmpeg -- --ignored
```

Of these:

- The unit tests cover:
  - manifests and forms;
  - the sandbox (globals removed, runaway code stopped, the memory cap);
  - permissions, shortcuts, targets and request coalescing.
- `plugins::tests` runs the bundled plugins: FFmpeg arguments, and git against a temporary repository.
- The ignored test converts a generated video with the real FFmpeg.
