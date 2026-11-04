-- Rust Clean: frees the disk space of a Rust project's build output, all of
-- it or only the incremental caches, which are the cheapest to rebuild.
--
-- Deleting is permanent: moving to the Trash wouldn't free anything, and the
-- worst outcome of a mistake here is a rebuild. The form shows exact paths and
-- sizes first, and the job works the paths out again in case they changed.

local cargo = require("lib.cargo")

local M = { actions = {} }

--- The form for a plan: what goes, how big it is, and any warnings.
---@param title string
---@param plan ClearPlan
---@param effect string What happens next, in one line.
---@return Form
local function form_for(title, plan, effect)
  local fields = {}
  for _, note in ipairs(plan.notes) do
    fields[#fields + 1] = { type = "info", text = note }
  end
  if plan.refusal then
    fields[#fields + 1] = { type = "info", text = plan.refusal }
    return { title = title, blocked = true, fields = fields }
  end
  local total, rows = 0, {}
  for _, path in ipairs(plan.paths) do
    local usage = dirfleet.fs.usage(path)
    -- What deleting frees: incremental caches share files with deps.
    local bytes = usage and usage.freed or 0
    total = total + bytes
    rows[#rows + 1] = { path = path, size = dirfleet.fmt.size(bytes) }
  end
  fields[#fields + 1] = { type = "paths", rows = rows }
  fields[#fields + 1] = { type = "info", text = effect }
  return { title = title, submit = "Delete " .. dirfleet.fmt.size(total), danger = true, fields = fields }
end

M.actions.clear_target = {
  form = function(ctx)
    return form_for(
      "Clear target for " .. dirfleet.path.name(ctx.root) .. "?",
      cargo.plan_target(ctx.root),
      "Deleted permanently. The next build recompiles everything, dependencies included."
    )
  end,

  validate = function(ctx)
    return cargo.plan_target(ctx.root).refusal
  end,

  run = function(ctx)
    local plan = cargo.plan_target(ctx.root)
    if plan.refusal then
      return { error = plan.refusal }
    end
    if not plan.cargo_clean then
      dirfleet.ops.delete(plan.paths)
      return
    end
    dirfleet.job.progress(0, "cargo clean")
    -- Waits for a running build to let go of the folder; Cancel stops it.
    local result, err = cargo.run(ctx.root, { "clean", "--offline" })
    if not result then
      return { error = err }
    end
    if not result.ok then
      return { error = cargo.last_line(result.stderr) or "cargo clean failed" }
    end
    dirfleet.app.refresh()
  end,
}

M.actions.clear_incremental = {
  form = function(ctx)
    return form_for(
      "Clear incremental for " .. dirfleet.path.name(ctx.root) .. "?",
      cargo.plan_incremental(ctx.root),
      "Deleted permanently. The next build recompiles this project's crates, but not its dependencies."
    )
  end,

  validate = function(ctx)
    return cargo.plan_incremental(ctx.root).refusal
  end,

  run = function(ctx)
    local plan = cargo.plan_incremental(ctx.root)
    if plan.refusal then
      return { error = plan.refusal }
    end
    dirfleet.ops.delete(plan.paths)
  end,
}

return M
