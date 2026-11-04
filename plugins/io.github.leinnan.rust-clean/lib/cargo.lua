-- Asking Cargo where a project's build output is, and deciding what may be
-- deleted. Nothing here deletes anything.

local M = {}

local ESCAPES = { b = "\b", f = "\f", n = "\n", r = "\r", t = "\t", ['"'] = '"', ["\\"] = "\\", ["/"] = "/" }

--- Reads the JSON string starting at the `"` at `start`.
---@return string? text
---@return integer? next The position after the closing quote.
local function read_string(text, start)
  local parts = {}
  local at = start + 1
  while true do
    local stop = text:find('["\\]', at)
    if not stop then
      return nil
    end
    parts[#parts + 1] = text:sub(at, stop - 1)
    if text:sub(stop, stop) == '"' then
      return table.concat(parts), stop + 1
    end
    local escape = text:sub(stop + 1, stop + 1)
    if escape == "u" then
      local code = tonumber(text:sub(stop + 2, stop + 5), 16)
      if not code then
        return nil
      end
      parts[#parts + 1] = utf8.char(code)
      at = stop + 6
    else
      parts[#parts + 1] = ESCAPES[escape] or escape
      at = stop + 2
    end
  end
end

--- The position after the JSON value at `at`.
---@return integer?
local function skip_value(json, at)
  local first = json:sub(at, at)
  if first == '"' then
    local _, after = read_string(json, at)
    return after
  end
  if first ~= "{" and first ~= "[" then
    -- A number, true, false or null.
    return json:find("[,}%]]", at)
  end
  local depth = 0
  while at do
    at = json:find('["{}%[%]]', at)
    if not at then
      return nil
    end
    local char = json:sub(at, at)
    if char == '"' then
      local _, after = read_string(json, at)
      at = after
    else
      depth = depth + ((char == "{" or char == "[") and 1 or -1)
      at = at + 1
      if depth == 0 then
        return at
      end
    end
  end
end

--- The string values of the outermost object's keys. Only the top level
--- counts: `[package.metadata]` can hold keys with the same names, and acting
--- on one of those could delete the wrong folder.
---@param json string
---@return table<string, string>
function M.top_level_strings(json)
  local found = {}
  local at = json:find("{", 1, true)
  at = at and at + 1
  while at do
    at = json:find("%S", at)
    local char = at and json:sub(at, at)
    if char == "," then
      at = at + 1
    elseif char ~= '"' then
      return found
    else
      local key, after = read_string(json, at)
      local colon = after and json:find("%S", after)
      if not colon or json:sub(colon, colon) ~= ":" then
        return found
      end
      local value = json:find("%S", colon + 1)
      if not value then
        return found
      end
      if json:sub(value, value) == '"' then
        local text, next_at = read_string(json, value)
        found[key] = text
        at = next_at
      else
        at = skip_value(json, value)
      end
    end
  end
  return found
end

--- Whether `path` is `base` or inside it.
function M.is_inside(path, base)
  local trimmed = base:gsub("[/\\]+$", "")
  if path == trimmed then
    return true
  end
  local next_char = path:sub(#trimmed + 1, #trimmed + 1)
  return path:sub(1, #trimmed) == trimmed and (next_char == "/" or next_char == "\\")
end

--- Runs cargo. An app started from the Dock or Finder doesn't have
--- ~/.cargo/bin on its PATH, so that is the second place to look.
--- A project's rust-toolchain file must not start a toolchain download.
---@param root string
---@param args string[]
---@param options { timeout: number? }?
---@return ProcessResult?, string?
function M.run(root, args, options)
  local function attempt(cmd)
    return dirfleet.process.run({
      cmd = cmd,
      args = args,
      cwd = root,
      env = { RUSTUP_AUTO_INSTALL = "0" },
      timeout = options and options.timeout,
    })
  end
  local result, err = attempt("cargo")
  if result then
    return result
  end
  local home = dirfleet.path.home()
  if home then
    local name = dirfleet.os == "windows" and "cargo.exe" or "cargo"
    local fallback = attempt(dirfleet.path.join(home, ".cargo", "bin", name))
    if fallback then
      return fallback
    end
  end
  return nil, err
end

--- The last meaningful line of what cargo printed.
function M.last_line(text)
  local line
  for each in (text or ""):gmatch("[^\r\n]+") do
    if each:find("%S") then
      line = each
    end
  end
  return line
end

---@class CargoPaths
---@field target string
---@field build string Where intermediate output goes; usually `target`.
---@field workspace string
---@field trusted boolean The paths came from cargo.
---@field problem string? Why cargo couldn't say.

--- Where the project's output is. Old projects are the likeliest to clean up
--- and the likeliest to have a broken manifest or a missing toolchain, so
--- when cargo can't tell, this falls back to `<root>/target`.
---@param root string
---@return CargoPaths
function M.locate(root)
  local result, err = M.run(root, { "metadata", "--no-deps", "--offline", "--format-version", "1" }, { timeout = 30 })
  local fields = result and result.ok and M.top_level_strings(result.stdout) or {}
  if fields.target_directory and fields.workspace_root then
    return {
      target = fields.target_directory,
      build = fields.build_directory or fields.target_directory,
      workspace = fields.workspace_root,
      trusted = true,
    }
  end
  local fallback = dirfleet.path.join(root, "target")
  local problem = err or (result and M.last_line(result.stderr)) or "cargo metadata gave no paths"
  return { target = fallback, build = fallback, workspace = root, trusted = false, problem = problem }
end

--- Cargo writes one or both of these into the folders it owns: CACHEDIR.TAG
--- since 2020, `.rustc_info.json` for much longer.
function M.is_cargo_output(dir)
  return dirfleet.fs.is_file(dirfleet.path.join(dir, "CACHEDIR.TAG"))
    or dirfleet.fs.is_file(dirfleet.path.join(dir, ".rustc_info.json"))
end

--- Folders of a profile that are never a profile's parent: `doc/incremental`
--- would be the documentation of a crate named "incremental".
local NOT_PROFILES = {
  deps = true,
  build = true,
  [".fingerprint"] = true,
  examples = true,
  doc = true,
  package = true,
  tmp = true,
}

--- The incremental caches under a build folder: real folders named exactly
--- `incremental`, at most 3 levels down, as in `debug/incremental` and
--- `wasm32-unknown-unknown/debug/incremental`.
---@param build string
---@return string[]
function M.find_incremental(build)
  local found = {}
  local function visit(dir, depth)
    -- read_dir doesn't follow symlinks: a link is neither is_dir nor is_file.
    for _, entry in ipairs(dirfleet.fs.read_dir(dir) or {}) do
      if entry.is_dir then
        if entry.name == "incremental" then
          if depth > 1 then
            found[#found + 1] = entry.path
          end
        elseif depth < 3 and not NOT_PROFILES[entry.name] then
          visit(entry.path, depth + 1)
        end
      end
    end
  end
  visit(build, 1)
  table.sort(found)
  return found
end

---@class ClearPlan
---@field paths string[] What would be deleted.
---@field refusal string? Why nothing may be deleted.
---@field notes string[] Worth saying before going ahead.
---@field cargo_clean boolean Delete with `cargo clean` rather than by path.

---@param root string
---@return ClearPlan
function M.plan_target(root)
  local where = M.locate(root)
  local plan = { paths = {}, notes = {}, cargo_clean = false }
  if not where.trusted then
    plan.notes[#plan.notes + 1] = "Cargo couldn't read this project (" .. where.problem .. "), so this uses " .. where.target .. "."
  elseif not M.is_inside(where.target, where.workspace) then
    plan.refusal = "The target folder " .. where.target .. " is outside this project, so other projects may share it. Clear it by hand."
    return plan
  elseif where.workspace ~= root then
    plan.notes[#plan.notes + 1] = "This project is part of the workspace " .. where.workspace .. ", which shares one target folder: all of it is cleared."
  end
  if not dirfleet.fs.is_dir(where.target) then
    plan.refusal = "There is nothing to clear: " .. where.target .. " doesn't exist."
    return plan
  end
  if not M.is_cargo_output(where.target) then
    plan.refusal = where.target .. " doesn't look like Cargo's output (no CACHEDIR.TAG or .rustc_info.json), so it stays."
    return plan
  end
  plan.paths = { where.target }
  plan.cargo_clean = where.trusted
  return plan
end

---@param root string
---@return ClearPlan
function M.plan_incremental(root)
  local where = M.locate(root)
  local plan = { paths = {}, notes = {}, cargo_clean = false }
  if not where.trusted then
    plan.notes[#plan.notes + 1] = "Cargo couldn't read this project (" .. where.problem .. "), so this looks in " .. where.build .. "."
  elseif not M.is_inside(where.build, where.workspace) then
    plan.notes[#plan.notes + 1] = "The build folder is outside this project, so other projects may share these caches."
  elseif where.workspace ~= root then
    plan.notes[#plan.notes + 1] = "This project is part of the workspace " .. where.workspace .. ", which shares these caches."
  end
  if not dirfleet.fs.is_dir(where.build) then
    plan.refusal = "There is nothing to clear: " .. where.build .. " doesn't exist."
    return plan
  end
  if not M.is_cargo_output(where.build) then
    plan.refusal = where.build .. " doesn't look like Cargo's output (no CACHEDIR.TAG or .rustc_info.json), so it stays."
    return plan
  end
  plan.paths = M.find_incremental(where.build)
  if #plan.paths == 0 then
    plan.refusal = "There is nothing to clear: no incremental caches in " .. where.build .. "."
  end
  return plan
end

return M
