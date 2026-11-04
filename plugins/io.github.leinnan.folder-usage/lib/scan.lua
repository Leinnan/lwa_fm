-- Choosing which folders get a row.

local M = {}

--- Build output and caches: not walked looking for projects, and their files
--- say nothing about when a project last changed.
M.build_folders = { "target", ".git", "node_modules" }

local function subfolders(dir)
  local entries, err = dirfleet.fs.read_dir(dir)
  if not entries then
    return nil, err
  end
  local folders = {}
  for _, entry in ipairs(entries) do
    if entry.is_dir then
      folders[#folders + 1] = entry
    end
  end
  table.sort(folders, function(a, b)
    return a.name:lower() < b.name:lower()
  end)
  return folders
end

M.subfolders = subfolders

--- Whether the name is a folder to leave out when looking for projects:
--- build output, or hidden like `.cargo` and `.venv`.
local function skipped(name)
  return name:sub(1, 1) == "." or name == "target" or name == "node_modules"
end

--- Rust projects under `dir`. The first Cargo.toml down a path is one project
--- and the search stops there, so workspace members don't get rows of their
--- own: they share the workspace's target folder.
---@param dir string
---@return string[] projects
---@return integer unreadable folders that could not be listed
function M.rust_projects(dir)
  local projects = {}
  local unreadable = 0
  local function visit(path)
    if dirfleet.job.cancelled() then
      return
    end
    if dirfleet.fs.is_file(dirfleet.path.join(path, "Cargo.toml")) then
      projects[#projects + 1] = path
      return
    end
    local folders = subfolders(path)
    if not folders then
      unreadable = unreadable + 1
      return
    end
    for _, folder in ipairs(folders) do
      if not skipped(folder.name) then
        visit(folder.path)
      end
    end
  end
  visit(dir)
  return projects, unreadable
end

--- Cargo writes one or both of these into the folders it owns: CACHEDIR.TAG
--- since 2020, `.rustc_info.json` for much longer.
function M.is_cargo_output(dir)
  return dirfleet.fs.is_file(dirfleet.path.join(dir, "CACHEDIR.TAG"))
    or dirfleet.fs.is_file(dirfleet.path.join(dir, ".rustc_info.json"))
end

--- A row's menu entries. The Cargo ones are Rust Clean's actions, which ask
--- Cargo where the output really is, so a workspace member's row clears the
--- workspace's folder, and they show what would go before anything does.
function M.menu(path)
  local name = dirfleet.path.name(path)
  local items = {}
  if dirfleet.fs.is_file(dirfleet.path.join(path, "Cargo.toml")) then
    items[#items + 1] = {
      label = "Clear incremental…",
      action = "io.github.leinnan.rust-clean.clear_incremental",
      danger = true,
      busy = "Clearing incremental…",
      title = "Clear incremental for " .. name .. "?",
    }
    items[#items + 1] = {
      label = "Clear target…",
      action = "io.github.leinnan.rust-clean.clear_target",
      danger = true,
      busy = "Clearing target…",
      title = "Clear target for " .. name .. "?",
    }
  end
  items[#items + 1] = {
    label = "Remove folder…",
    action = "io.github.leinnan.folder-usage.remove_folder",
    danger = true,
    busy = "Removing…",
    title = "Remove " .. name .. "?",
  }
  return items
end

--- Why `path` must not be removed: the root of a disk, or a folder holding
--- the user's home.
function M.removal_refusal(path)
  if not dirfleet.path.parent(path) then
    return path .. " is the root of a disk, so it stays."
  end
  local home = dirfleet.path.home()
  local base = path:gsub("[/\\]+$", "")
  if home then
    local after = home:sub(#base + 1, #base + 1)
    if home == base or (home:sub(1, #base) == base and (after == "/" or after == "\\")) then
      return path .. " holds your home folder, so it stays."
    end
  end
end

--- `path` relative to `base`, for display.
function M.relative(path, base)
  if path == base then
    return dirfleet.path.name(path)
  end
  local prefix = base:gsub("[/\\]+$", "")
  if path:sub(1, #prefix) == prefix then
    local rest = path:sub(#prefix + 1):gsub("^[/\\]+", "")
    if rest ~= "" then
      return rest
    end
  end
  return path
end

return M
