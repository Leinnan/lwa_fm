---@meta
-- DirFleet plugin API, version 1. Editors with the Lua language server
-- (LuaLS) use this file for completion; DirFleet keeps a copy in the
-- plugins folder. See docs/plugins.md for the full guide.

---@class Target An item an action runs on.
---@field path string Absolute path.
---@field name string File name with extension.
---@field stem string Name without the extension.
---@field ext string Lowercase extension without the dot ("" for folders).
---@field is_dir boolean
---@field size integer Bytes.
---@field modified integer Seconds since 1970.

---@class Context What a handler is called with.
---@field dir string? The folder shown, when one folder is shown.
---@field root string? With `ancestor_contains` activation: the folder that matched (e.g. the repository root).
---@field targets Target[] The selected items the action applies to.
---@field tab_id integer?
---@field values table<string, any> Submitted form values.
---@field plugin { id: string, name: string, version: string }

---@alias FieldType "info"|"paths"|"separator"|"group"|"text"|"multiline"|"int"|"float"|"bool"|"choice"|"radio"|"time"|"path"

---@class Condition Compares another field's current value.
---@field field string
---@field equals any?
---@field not_equals any?
---@field in any[]?
---@field not_in any[]?

---@class Field
---@field type FieldType
---@field id string? Required for fields with a value.
---@field label string?
---@field text string? For `info`.
---@field default any?
---@field required boolean?
---@field help string? Tooltip.
---@field placeholder string?
---@field enabled boolean?
---@field visible_if Condition?
---@field enabled_if Condition?
---@field min number?
---@field max number? For `time`: seconds.
---@field step number?
---@field slider boolean? `int`/`float` with `min` and `max`: draw a slider.
---@field options (string|string[]|{value: string, label: string})[]? For `choice` and `radio`: `{ "value", "Label" }`.
---@field fields Field[]? For `group`.
---@field rows { path: string, size: string? }[]? For `paths`.
---@field mode "file"|"save"|"folder"|nil For `path`.

---@class Form
---@field title string?
---@field submit string? The button's text (default "Run").
---@field danger boolean? The button is red, for what can't be undone.
---@field submit_when { when: Condition, label: string, danger: boolean? }[]? Another look for the button while a condition holds.
---@field blocked boolean? There is nothing to submit, as the fields explain: only Close is offered.
---@field remember boolean? Start from the values used last time.
---@field fields Field[]

---@class BarSegment
---@field text string?
---@field badge string?
---@field button string? A button running `action`.
---@field action string? An action id of this plugin.
---@field icon string? A lucide icon name, e.g. "git-branch".
---@field tooltip string?
---@field tone "normal"|"muted"|"good"|"warn"|"bad"|nil
---@field spinner boolean?

---@alias InspectorRow string[]|{label: string, value: string}

---@class ProcessOptions
---@field cmd string The program; needs the "process:<name>" permission.
---@field args (string|number)[]?
---@field cwd string?
---@field env table<string, string>?
---@field timeout number? Seconds. Default: 30 in quick handlers, none in jobs.
---@field stdin string?
---@field on_line fun(line: string)? Called with each line of output.

---@class ProcessResult
---@field code integer?
---@field ok boolean The exit code was 0.
---@field stdout string
---@field stderr string

---@class MediaInfo
---@field duration number? Seconds.
---@field width integer?
---@field height integer?
---@field fps number?
---@field video_codec string?
---@field audio_codec string?
---@field has_video boolean
---@field has_audio boolean
---@field rotation integer?
---@field format string?
---@field bit_rate integer?

---@class FileStat
---@field path string
---@field name string
---@field is_dir boolean
---@field is_file boolean
---@field size integer
---@field modified integer
---@field readonly boolean

---@class FolderUsage
---@field bytes integer Disk space taken, with hardlinks counted once (on Windows, adds up file sizes).
---@field freed integer What deleting the folder would free: `bytes`, less files also linked from outside it.
---@field files integer
---@field newest integer? Newest file's modification time, seconds since 1970.
---@field errors integer Entries that could not be read.
---@field parts table<string, integer> Bytes of the folders asked for with `subtotals`.

---@class UsageOptions
---@field newest_skip string[]? Folder names whose files don't count towards `newest`, wherever they are.
---@field subtotals string[]? Folders directly under the root to add up separately, in the same walk.

---@class UsageManyOptions : UsageOptions
---@field paths string[]
---@field on_result fun(path: string, usage: FolderUsage?, error: string?) Called as each folder finishes, in no set order.

---@class TableColumn
---@field id string
---@field label string
---@field kind "text"|"size"|"time"|nil `size` is bytes, `time` is seconds since 1970. Default `text`.

---@class TableRow
---@field path string? Double-clicking the row opens this folder in a new tab.
---@field cells table<string, string|number> By column id. A missing cell is blank and sorts last.
---@field menu TableRowAction[]? More entries for the row's context menu.

---@class TableRowAction
---@field label string
---@field action string The full id of an action to start on the row's folder, which should ask before removing anything.
---@field danger boolean? Draw it as a warning.
---@field busy string? What the row says while this runs.
---@field title string? The confirmation's title, shown while the plugin prepares it.

---@class TableRowUpdate
---@field path string The row to change.
---@field cells table<string, string|number>? Cells to set; others keep their values.
---@field clear string[]? Cells to blank.
---@field remove boolean? The folder is gone: drop the row.

---@class TableOptions
---@field title string
---@field subtitle string? `{count}` and `{total}` are filled in from the rows, and follow them.
---@field refresh_action string? An action of this plugin that measures one folder again, run with the row's folder.
---@field progress string? What is still happening, such as "Scanning 12 of 136". Leave it out of the last update.
---@field columns TableColumn[]
---@field rows TableRow[]?
---@field sort { column: string, descending: boolean? }?

dirfleet = {}

---@type string The app's version.
dirfleet.version = ""
---@type integer
dirfleet.api_version = 1
---@type "windows"|"macos"|"linux"
dirfleet.os = "windows"

--- Shows a notification.
---@param text string
---@param level "info"|"success"|"warning"|"error"|nil
function dirfleet.notify(text, level) end

dirfleet.path = {}
---@param ... string
---@return string
function dirfleet.path.join(...) end
---@return string?
function dirfleet.path.parent(path) end
---@return string
function dirfleet.path.name(path) end
---@return string
function dirfleet.path.stem(path) end
---@return string
function dirfleet.path.ext(path) end
--- `with_ext("/a/clip.mov", "mp4")` is "/a/clip.mp4".
---@return string
function dirfleet.path.with_ext(path, ext) end
---@return boolean
function dirfleet.path.is_absolute(path) end
--- `path` when free, else "name (2).ext", "name (3).ext"…
---@return string
function dirfleet.path.unique(path) end
--- The user's home folder.
---@return string?
function dirfleet.path.home() end

dirfleet.fmt = {}
--- 75 → "1:15", 3725 → "1:02:05".
---@param seconds number?
---@return string?
function dirfleet.fmt.duration(seconds) end
---@param bytes integer
---@return string
function dirfleet.fmt.size(bytes) end

--- Needs "fs.read" (reading) or "fs.write" (writing). Paths must be absolute.
dirfleet.fs = {}
---@return boolean
function dirfleet.fs.exists(path) end
---@return boolean
function dirfleet.fs.is_dir(path) end
---@return boolean
function dirfleet.fs.is_file(path) end
---@return FileStat?, string?
function dirfleet.fs.stat(path) end
---@return FileStat[]?, string?
function dirfleet.fs.read_dir(path) end
--- Reads up to `max_bytes` (1 MB by default); the second result says whether it was cut short.
---@return string?, boolean|string
function dirfleet.fs.read_text(path, max_bytes) end
---@return string[]?, string?
function dirfleet.fs.glob(pattern) end
--- Walks a folder for its real disk use, without following symlinks or leaving
--- the filesystem.
---@param options UsageOptions?
---@return FolderUsage?, string?
function dirfleet.fs.usage(path, options) end
--- `usage` for several folders at once, on threads of its own.
--- Returns true, or nil and "cancelled".
---@param options UsageManyOptions
---@return true?, string?
function dirfleet.fs.usage_many(options) end
---@return true?, string?
function dirfleet.fs.write_text(path, text) end
---@return true?, string?
function dirfleet.fs.mkdir(path) end

--- File operations through the app: with progress, conflict handling and
--- undo. Need "fs.write".
dirfleet.ops = {}
---@param srcs string|string[]
---@param conflict "keep_both"|"replace"|"skip"|nil
function dirfleet.ops.copy(srcs, dst, conflict) end
---@param srcs string|string[]
---@param conflict "keep_both"|"replace"|"skip"|nil
function dirfleet.ops.move(srcs, dst, conflict) end
---@param paths string|string[]
function dirfleet.ops.trash(paths) end
--- Deletes permanently, with no undo. Returns at once: the app reports the outcome.
---@param paths string|string[]
function dirfleet.ops.delete(paths) end
--- `to` is a new name, or an absolute path.
function dirfleet.ops.rename(from, to) end

dirfleet.process = {}
---@param options ProcessOptions
---@return ProcessResult?, string?
function dirfleet.process.run(options) end

--- The FFmpeg the app uses for previews. Needs "ffmpeg".
dirfleet.ffmpeg = {}
---@return boolean
function dirfleet.ffmpeg.available() end
--- Runs FFmpeg with `args` (after -hide_banner -nostdin -progress pipe:1).
--- With `duration` (seconds), progress reaches the job and `on_progress`.
---@param options { args: string[], duration: number?, on_progress: fun(fraction: number)?, timeout: number? }
---@return { code: integer?, ok: boolean, stderr: string }?, string?
function dirfleet.ffmpeg.run(options) end
---@return string?
function dirfleet.ffmpeg.version() end
--- Encoder names as keys: `encoders()["libx264"]`. Empty while FFmpeg is missing.
---@return table<string, boolean>
function dirfleet.ffmpeg.encoders() end

dirfleet.media = {}
--- Needs "ffmpeg".
---@return MediaInfo?, string?
function dirfleet.media.probe(path) end

dirfleet.app = {}
function dirfleet.app.open(path) end
function dirfleet.app.open_in_new_tab(path) end
function dirfleet.app.refresh() end
---@param paths string|string[]
function dirfleet.app.select(paths) end
--- Opens a document with its default app (not programs). Needs "fs.read".
function dirfleet.app.system_open(path) end
function dirfleet.app.open_terminal(dir) end
--- Shows or updates the results in the dialog of the job's action, which needs
--- `results = true` in plugin.toml. Sorted by clicking the headers.
---@param options TableOptions
function dirfleet.app.show_table(options) end
--- Changes one row of this plugin's results.
---@param update TableRowUpdate
function dirfleet.app.update_table_row(update) end

dirfleet.clipboard = {}
--- Needs "clipboard".
function dirfleet.clipboard.set_text(text) end

dirfleet.log = {}
function dirfleet.log.info(...) end
function dirfleet.log.warn(...) end
--- Also listed under the plugin's errors in Settings.
function dirfleet.log.error(...) end

--- In jobs: progress and cancellation.
dirfleet.job = {}
---@param fraction number 0..1 (of the current item, for per_item actions)
---@param label string?
function dirfleet.job.progress(fraction, label) end
---@return boolean
function dirfleet.job.cancelled() end

--- A small JSON store kept per plugin.
dirfleet.storage = {}
---@return any
function dirfleet.storage.get(key) end
function dirfleet.storage.set(key, value) end

dirfleet.bar = {}
--- Asks for the bar again soon.
function dirfleet.bar.invalidate() end
dirfleet.inspector = {}
function dirfleet.inspector.invalidate() end
