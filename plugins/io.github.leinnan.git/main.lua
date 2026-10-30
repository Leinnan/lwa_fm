-- Git: the branch and changes of the repository being browsed, in the
-- directory bar, plus fetch, pull, push and commit.

local M = { actions = {} }

local function trim(text)
  return ((text or ""):gsub("^%s+", ""):gsub("%s+$", ""))
end

--- Runs git in `dir`. Never asks for credentials: a prompt would hang.
---@param dir string
---@param args string[]
---@param on_line? fun(line: string)
---@return ProcessResult
local function git(dir, args, on_line)
  local result, err = dirfleet.process.run({
    cmd = "git",
    args = args,
    cwd = dir,
    env = { GIT_TERMINAL_PROMPT = "0", GIT_OPTIONAL_LOCKS = "0" },
    on_line = on_line,
  })
  if not result then
    error(err, 0)
  end
  return result
end

--- Why git failed, in one line.
local function failure(result)
  local text = trim(result.stderr ~= "" and result.stderr or result.stdout)
  return text ~= "" and text or "git failed"
end

---@class GitStatus
---@field branch string
---@field upstream string?
---@field ahead integer
---@field behind integer
---@field staged integer
---@field changed integer
---@field untracked integer
---@field conflicts integer

---@return GitStatus?, string?
local function status(dir)
  local result = git(dir, { "status", "--porcelain=v2", "--branch" })
  if not result.ok then
    return nil, failure(result)
  end
  local s = { branch = "?", ahead = 0, behind = 0, staged = 0, changed = 0, untracked = 0, conflicts = 0 }
  for line in result.stdout:gmatch("[^\n]+") do
    s.branch = line:match("^# branch%.head (.+)$") or s.branch
    s.upstream = line:match("^# branch%.upstream (.+)$") or s.upstream
    local ahead, behind = line:match("^# branch%.ab %+(%d+) %-(%d+)$")
    if ahead then
      s.ahead, s.behind = tonumber(ahead), tonumber(behind)
    end
    local kind = line:sub(1, 1)
    if kind == "1" or kind == "2" then
      if line:sub(3, 3) ~= "." then s.staged = s.staged + 1 end
      if line:sub(4, 4) ~= "." then s.changed = s.changed + 1 end
    elseif kind == "u" then
      s.conflicts = s.conflicts + 1
    elseif kind == "?" then
      s.untracked = s.untracked + 1
    end
  end
  return s
end

---@param ctx Context
---@return BarSegment[]
function M.bar(ctx)
  local s, err = status(ctx.dir)
  if not s then
    return { { text = "git: " .. err, tone = "bad", icon = "git-branch" } }
  end
  local branch = s.branch == "(detached)" and "detached HEAD" or s.branch
  local segments = {
    {
      text = branch,
      icon = "git-branch",
      tooltip = s.upstream and ("Tracking " .. s.upstream) or "No upstream branch",
    },
  }
  local function add(segment)
    segments[#segments + 1] = segment
  end
  if s.ahead > 0 then
    add({ text = tostring(s.ahead), icon = "arrow-up", tooltip = s.ahead .. " commit(s) to push" })
  end
  if s.behind > 0 then
    add({ text = tostring(s.behind), icon = "arrow-down", tone = "warn", tooltip = s.behind .. " commit(s) to pull" })
  end
  if s.conflicts > 0 then
    add({ badge = s.conflicts .. " conflicts", tone = "bad" })
  end
  local dirty = s.staged + s.changed + s.untracked
  if dirty > 0 then
    add({
      badge = dirty .. " changed",
      tone = "warn",
      tooltip = string.format("%d staged, %d modified, %d untracked", s.staged, s.changed, s.untracked),
    })
  else
    add({ text = "clean", tone = "muted" })
  end
  add({ button = "Fetch", action = "fetch", icon = "refresh-cw", tooltip = "git fetch --prune" })
  if s.upstream then
    add({ button = "Pull", action = "pull", icon = "arrow-down-to-line", tooltip = "git pull --ff-only" })
    add({ button = "Push", action = "push", icon = "arrow-up-from-line", tooltip = "git push" })
  end
  if dirty > 0 then
    add({ button = "Commit…", action = "commit", icon = "git-commit-horizontal" })
  end
  return segments
end

--- An action running one git command and reporting its output as progress.
local function sync(args)
  return function(ctx)
    local result = git(ctx.dir, args, function(line)
      dirfleet.job.progress(0.5, line)
    end)
    if not result.ok then
      return { error = failure(result) }
    end
  end
end

M.actions.fetch = sync({ "fetch", "--prune" })
M.actions.pull = sync({ "pull", "--ff-only" })
M.actions.push = sync({ "push" })

M.actions.commit = {
  ---@param ctx Context
  ---@return Form
  form = function(ctx)
    local result = git(ctx.dir, { "status", "--porcelain" })
    local files = {}
    for line in result.stdout:gmatch("[^\n]+") do
      files[#files + 1] = line
    end
    local shown = table.concat(files, "\n", 1, math.min(#files, 15))
    if #files > 15 then
      shown = shown .. "\n… and " .. (#files - 15) .. " more"
    end
    return {
      title = "Commit",
      submit = "Commit",
      fields = {
        { type = "info", text = #files == 0 and "Nothing to commit." or shown },
        { id = "message", type = "multiline", label = "Message", required = true, placeholder = "What changed and why" },
        { id = "all", type = "bool", label = "Stage all changes", default = true, help = "git add --all" },
        { id = "push", type = "bool", label = "Push afterwards", default = false },
      },
    }
  end,
  validate = function(ctx, values)
    if trim(values.message) == "" then
      return { message = "Write a commit message" }
    end
  end,
  run = function(ctx, values)
    if values.all then
      local added = git(ctx.dir, { "add", "--all" })
      if not added.ok then
        return { error = failure(added) }
      end
    end
    local committed = git(ctx.dir, { "commit", "-m", values.message })
    if not committed.ok then
      return { error = failure(committed) }
    end
    if values.push then
      local pushed = git(ctx.dir, { "push" })
      if not pushed.ok then
        return { error = "committed, but the push failed: " .. failure(pushed) }
      end
    end
  end,
}

local STATES = {
  ["M"] = "Modified",
  ["A"] = "Added",
  ["D"] = "Deleted",
  ["R"] = "Renamed",
  ["C"] = "Copied",
  ["U"] = "Conflict",
  ["?"] = "Untracked",
  ["!"] = "Ignored",
}

---@param ctx Context
---@return InspectorRow[]?
function M.inspector(ctx)
  local target = ctx.targets[1]
  if not target then
    return nil
  end
  local dir = target.is_dir and target.path or dirfleet.path.parent(target.path)
  local result = git(dir, { "status", "--porcelain", "--ignored", "--", target.path })
  if not result.ok then
    return nil -- not in a repository
  end
  local code = result.stdout:sub(1, 2)
  local state = "Unchanged"
  if code ~= "" then
    local staged, unstaged = STATES[code:sub(1, 1)], STATES[code:sub(2, 2)]
    if code == "??" or code == "!!" then
      state = STATES[code:sub(1, 1)]
    elseif staged and unstaged then
      state = staged .. " (staged), " .. unstaged:lower()
    elseif staged then
      state = staged .. " (staged)"
    else
      state = unstaged or code
    end
  end
  local rows = { { "Status", state } }
  local log = git(dir, { "log", "-1", "--format=%h%x09%an%x09%ar%x09%s", "--", target.path })
  local hash, author, date, subject = log.stdout:match("^(%S+)\t([^\t]*)\t([^\t]*)\t([^\n]*)")
  if hash then
    rows[#rows + 1] = { "Last commit", subject }
    rows[#rows + 1] = { "By", author .. ", " .. date .. " (" .. hash .. ")" }
  end
  return rows
end

return M
