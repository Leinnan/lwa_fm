-- Video Converter: "Convert to MP4" turns clip.mov into clip.mp4 next to
-- it; "Convert video…" asks for the format, quality, size and fragment.

local args = require("lib.args")

local M = { actions = {} }

--- Where the output goes, or nil to skip. Never the input itself.
---@return string?, boolean
local function output_for(input, ext, exists)
  local output = dirfleet.path.with_ext(input, ext)
  local same = output:lower() == input:lower()
  if same or dirfleet.fs.exists(output) then
    if not same and exists == "skip" then
      return nil, false
    end
    if not same and exists == "overwrite" then
      return output, true
    end
    output = dirfleet.path.unique(output)
  end
  return output, false
end

--- The last line of FFmpeg's error output.
local function last_line(text)
  local line
  for each in (text or ""):gmatch("[^\r\n]+") do
    line = each
  end
  return line or "FFmpeg failed"
end

---@param ctx Context
---@param item Target
---@param o ConvertOptions
---@param select boolean
local function convert(ctx, item, o, exists, select)
  local format = args.format(o.format)
  if not format then
    return { error = "unknown format " .. tostring(o.format) }
  end
  local output, overwrite = output_for(item.path, format.ext, exists)
  if not output then
    return { skipped = true }
  end
  o.overwrite = overwrite
  local info = dirfleet.media.probe(item.path)
  local duration = info and info.duration
  if o.finish then
    duration = o.finish - (o.start or 0)
  elseif o.start and duration then
    duration = duration - o.start
  end
  local result, err = dirfleet.ffmpeg.run({ args = args.build(item.path, output, o), duration = duration })
  if not result then
    return { error = err }
  end
  if not result.ok then
    return { error = last_line(result.stderr) }
  end
  if select and #ctx.targets == 1 then
    dirfleet.app.select(output)
  end
end

M.actions.to_mp4 = {
  run_item = function(ctx, item)
    return convert(ctx, item, { format = "mp4", quality = "balanced", scale = "src", audio = true }, "rename", true)
  end,
}

M.actions.convert = {
  ---@param ctx Context
  ---@return Form
  form = function(ctx)
    local single = #ctx.targets == 1
    local first = ctx.targets[1]
    local info = dirfleet.media.probe(first.path)
    local summary
    if single and info then
      local parts = {}
      if info.width then
        parts[#parts + 1] = info.width .. "×" .. info.height
      end
      if info.video_codec then
        parts[#parts + 1] = info.video_codec .. (info.audio_codec and (" + " .. info.audio_codec) or "")
      end
      if info.fps then
        parts[#parts + 1] = string.format("%.3g fps", info.fps)
      end
      if info.duration then
        parts[#parts + 1] = dirfleet.fmt.duration(info.duration)
      end
      summary = table.concat(parts, " · ")
    elseif single and not dirfleet.ffmpeg.available() then
      summary = "FFmpeg will be downloaded the first time it's needed."
    elseif single then
      summary = "Couldn't read this video's details."
    else
      summary = #ctx.targets .. " videos, each converted next to the original."
    end

    -- Offer only what this FFmpeg can encode (all, when it isn't known yet).
    local encoders = dirfleet.ffmpeg.encoders()
    local known = next(encoders) ~= nil
    local formats = {}
    for _, format in ipairs(args.formats) do
      if not known or encoders[format.encoder] then
        formats[#formats + 1] = { format.value, format.label }
      end
    end

    local duration = info and info.duration
    local can_cut = single and duration ~= nil
    return {
      title = single and ("Convert " .. first.name) or ("Convert " .. #ctx.targets .. " videos"),
      submit = "Convert",
      remember = true,
      fields = {
        { type = "info", text = summary },
        { id = "format", type = "choice", label = "Format", options = formats, default = "mp4" },
        {
          id = "quality",
          type = "choice",
          label = "Quality",
          options = { { "fast", "Smaller, faster" }, { "balanced", "Balanced" }, { "high", "Higher quality" } },
          default = "balanced",
          visible_if = { field = "format", not_in = { "mp3", "gif" } },
        },
        {
          id = "scale",
          type = "choice",
          label = "Height",
          options = { { "src", "Same as the source" }, { "1080", "1080p" }, { "720", "720p" }, { "480", "480p" } },
          default = "src",
          visible_if = { field = "format", not_equals = "mp3" },
        },
        {
          id = "range",
          type = "radio",
          label = "Convert",
          options = { { "all", "Whole video" }, { "part", "A fragment" } },
          default = "all",
          enabled = can_cut,
          help = not can_cut and "Fragments need a single video with a known length" or nil,
        },
        {
          id = "start",
          type = "time",
          label = "From",
          default = 0,
          min = 0,
          max = duration,
          visible_if = { field = "range", equals = "part" },
        },
        {
          id = "finish",
          type = "time",
          label = "To",
          default = duration or 0,
          min = 0,
          max = duration,
          visible_if = { field = "range", equals = "part" },
        },
        {
          id = "audio",
          type = "bool",
          label = "Keep audio",
          default = true,
          visible_if = { field = "format", not_in = { "mp3", "gif" } },
        },
        {
          id = "exists",
          type = "choice",
          label = "If the file exists",
          options = { { "rename", "Keep both" }, { "skip", "Skip" }, { "overwrite", "Replace it" } },
          default = "rename",
        },
        { id = "select", type = "bool", label = "Select the result", default = true },
      },
    }
  end,

  validate = function(ctx, v)
    if #ctx.targets == 1 and v.range == "part" and v.finish <= v.start then
      return { finish = "Must be after the start" }
    end
  end,

  run_item = function(ctx, item, v)
    local o = { format = v.format, quality = v.quality, scale = v.scale, audio = v.audio }
    if v.range == "part" and #ctx.targets == 1 then
      o.start, o.finish = v.start, v.finish
    end
    return convert(ctx, item, o, v.exists, v.select)
  end,
}

---@param ctx Context
---@return InspectorRow[]?
function M.inspector(ctx)
  local info = dirfleet.media.probe(ctx.targets[1].path)
  if not info then
    return nil
  end
  local rows = {}
  local function add(label, value)
    if value then
      rows[#rows + 1] = { label, value }
    end
  end
  add("Duration", info.duration and dirfleet.fmt.duration(info.duration))
  add("Resolution", info.width and (info.width .. " × " .. info.height))
  add("Frame rate", info.fps and string.format("%.3g fps", info.fps))
  add("Video", info.video_codec)
  add("Audio", info.audio_codec or (info.has_video and "None") or nil)
  add("Bit rate", info.bit_rate and string.format("%d kb/s", info.bit_rate // 1000))
  return rows
end

return M
