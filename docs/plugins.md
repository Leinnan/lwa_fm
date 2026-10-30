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

Field types:

| Type | Notes |
| --- | --- |
| `info` | Read-only text, from `text`. |
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
| `dirfleet.path` | `join`, `parent`, `name`, `stem`, `ext`, `with_ext`, `is_absolute`, `unique` ("name (2).ext") |
| `dirfleet.fmt` | `duration(seconds)`, `size(bytes)` |
| `dirfleet.fs` | `exists`, `is_dir`, `is_file`, `stat`, `read_dir`, `read_text`, `glob`, `write_text`, `mkdir` (absolute paths only) |
| `dirfleet.ops` | `copy`, `move`, `trash`, `rename`: through the app's file operations, with progress and undo |
| `dirfleet.process` | `run{ cmd, args, cwd, env, timeout, stdin, on_line }` returns `{ code, ok, stdout, stderr }` |
| `dirfleet.ffmpeg` | `run{ args, duration, on_progress }`, `available`, `version`, `encoders` |
| `dirfleet.media` | `probe(path)` returns `{ duration, width, height, fps, video_codec, audio_codec, has_audio, … }` |
| `dirfleet.app` | `open`, `open_in_new_tab`, `refresh`, `select`, `system_open`, `open_terminal` |
| `dirfleet.job` | `progress(fraction, label)`, `cancelled()` |
| `dirfleet.storage` | `get`, `set`: a JSON file per plugin |
| `dirfleet.bar`, `dirfleet.inspector` | `invalidate()` |
| other | `dirfleet.notify(text, level)`, `dirfleet.log.info/warn/error`, `dirfleet.clipboard.set_text` |

How errors come back:

- A missing permission raises an error that names it.
- A failed operation returns `nil, message`.
- `print` writes to the log.

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
