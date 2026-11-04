-- Folder Usage: a table of the folders under the current one, or of the Rust
-- projects anywhere below it, with their real disk use and last change.
--
-- Sizes come from fs.usage, which counts hardlinks once: adding up file sizes
-- reads about twice the truth on a Cargo target folder. "Last change" is the
-- newest file outside build output, since building touches target and a
-- folder's own date misses edits further down.

local scan = require("lib.scan")

local M = { actions = {} }

---@param ctx Context
---@param values table
local function run(ctx, values)
  local dir = ctx.dir
  local paths, unreadable = {}, 0
  if values.show == "rust" then
    dirfleet.job.progress(0, "Looking for Rust projects…")
    paths, unreadable = scan.rust_projects(dir)
  else
    local folders, err = scan.subfolders(dir)
    if not folders then
      return { error = err }
    end
    for _, folder in ipairs(folders) do
      paths[#paths + 1] = folder.path
    end
  end

  local rows, by_path = {}, {}
  for _, path in ipairs(paths) do
    local row = { path = path, cells = { name = scan.relative(path, dir) }, menu = scan.menu(path) }
    rows[#rows + 1] = row
    by_path[path] = row
  end

  local done = 0
  local function show()
    local noun = values.show == "rust" and "Projects" or "Folders"
    local subtitle = noun .. ": {count} · {total}"
    if unreadable > 0 then
      subtitle = subtitle .. ". " .. unreadable .. (unreadable == 1 and " item" or " items") .. " could not be read"
    end
    dirfleet.app.show_table({
      title = "Folder Usage: " .. dirfleet.path.name(dir),
      subtitle = subtitle,
      progress = done < #rows and ("Scanning " .. done .. " of " .. #rows) or nil,
      refresh_action = "refresh_row",
      columns = {
        { id = "name", label = "Folder" },
        { id = "size", label = "Size", kind = "size" },
        { id = "target", label = "target", kind = "size" },
        { id = "modified", label = "Last change", kind = "time" },
      },
      rows = rows,
      sort = { column = "size", descending = true },
    })
  end
  show()

  -- Enough updates to watch it fill in, without copying a long list each time.
  local every = math.max(1, #rows // 50)
  local finished, err = dirfleet.fs.usage_many({
    paths = paths,
    newest_skip = scan.build_folders,
    subtotals = { "target" },
    on_result = function(path, usage)
      done = done + 1
      local cells = by_path[path].cells
      dirfleet.job.progress(done / #rows, cells.name)
      if usage then
        unreadable = unreadable + usage.errors
        cells.size = usage.bytes
        cells.modified = usage.newest
        if scan.is_cargo_output(dirfleet.path.join(path, "target")) then
          cells.target = usage.parts.target
        end
      else
        unreadable = unreadable + 1
      end
      if done < #rows and done % every == 0 then
        show()
      end
    end,
  })
  if not finished then
    return err == "cancelled" and { skipped = true } or { error = err }
  end
  show()
end

M.actions.scan = { run = run }

--- One folder measured again, after something changed it.
M.actions.refresh_row = {
  run = function(ctx)
    local path = ctx.dir
    if not dirfleet.fs.exists(path) then
      dirfleet.app.update_table_row({ path = path, remove = true })
      return
    end
    local usage, err = dirfleet.fs.usage(path, { newest_skip = scan.build_folders, subtotals = { "target" } })
    if not usage then
      return err == "cancelled" and { skipped = true } or { error = err }
    end
    local target = scan.is_cargo_output(dirfleet.path.join(path, "target")) and usage.parts.target or nil
    local clear = {}
    if not usage.newest then
      clear[#clear + 1] = "modified"
    end
    if not target then
      clear[#clear + 1] = "target"
    end
    dirfleet.app.update_table_row({
      path = path,
      cells = { size = usage.bytes, modified = usage.newest, target = target },
      clear = clear,
    })
  end,
}

--- The whole folder, sources included, so the Trash is the default and the
--- dialog says exactly what goes.
M.actions.remove_folder = {
  form = function(ctx)
    local path = ctx.dir
    local title = "Remove " .. dirfleet.path.name(path) .. "?"
    local refusal = scan.removal_refusal(path)
    if refusal then
      return { title = title, blocked = true, fields = { { type = "info", text = refusal } } }
    end
    local usage = dirfleet.fs.usage(path)
    local size = usage and dirfleet.fmt.size(usage.freed) or "unknown size"
    local fields = {
      { type = "info", text = "This whole folder and everything in it:" },
      { type = "paths", rows = { { path = path, size = size } } },
    }
    if usage and usage.errors > 0 then
      fields[#fields + 1] = { type = "info", text = usage.errors .. " items in it could not be read, so it may be larger." }
    end
    if dirfleet.fs.exists(dirfleet.path.join(path, ".git")) then
      fields[#fields + 1] = { type = "info", text = "It has a git repository: unpushed commits and uncommitted changes go with it." }
    end
    fields[#fields + 1] = {
      id = "how",
      type = "radio",
      label = "How",
      options = { { "trash", "Move to Trash" }, { "delete", "Delete permanently" } },
      default = "trash",
      help = "The Trash can restore it, but the disk space comes back only when it is emptied.",
    }
    return {
      title = title,
      submit = "Move to Trash",
      submit_when = {
        { when = { field = "how", equals = "delete" }, label = "Delete " .. size, danger = true },
      },
      fields = fields,
    }
  end,

  validate = function(ctx)
    return scan.removal_refusal(ctx.dir)
  end,

  run = function(ctx, values)
    local refusal = scan.removal_refusal(ctx.dir)
    if refusal then
      return { error = refusal }
    end
    if values.how == "delete" then
      dirfleet.ops.delete(ctx.dir)
    else
      dirfleet.ops.trash(ctx.dir)
    end
  end,
}

return M
