-- HEIC Converter: "Convert to JPEG" turns IMG_0001.HEIC into IMG_0001.jpg
-- next to it; "Convert photo…" asks for the format, quality and size.
--
-- On macOS sips does the work and keeps the photo's metadata. Elsewhere it is
-- FFmpeg, which drops the capture date and location from JPEG and TIFF.

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

--- The photo's longest side, from sips.
---@return integer?
local function sips_longest_side(path)
  local result = dirfleet.process.run({ cmd = "sips", args = { "-g", "pixelWidth", "-g", "pixelHeight", path } })
  if not result or not result.ok then
    return nil
  end
  local width = tonumber(result.stdout:match("pixelWidth: (%d+)"))
  local height = tonumber(result.stdout:match("pixelHeight: (%d+)"))
  return width and height and math.max(width, height)
end

---@param o PhotoOptions
---@return string? error
local function with_sips(input, output, o)
  local side = tonumber(o.side)
  local longest = side and sips_longest_side(input)
  if not (longest and longest > side) then
    o.side = nil
  end
  local result, err = dirfleet.process.run({ cmd = "sips", args = args.sips(input, output, o) })
  if not result then
    return err
  end
  if not result.ok then
    return (result.stderr .. "\n" .. result.stdout):match("Error[^\r\n]*") or "sips failed"
  end
  if not dirfleet.fs.exists(output) then
    return "sips wrote no file"
  end
end

---@param o PhotoOptions
---@return string? error
local function with_ffmpeg(input, output, o)
  local version = dirfleet.ffmpeg.version()
  if not args.ffmpeg_supported(version) then
    return "Converting photos needs FFmpeg 8.1 or newer; found " .. version
  end
  o.grid = tonumber(o.side) ~= nil
  local result, err = dirfleet.ffmpeg.run({ args = args.ffmpeg(input, output, o) })
  -- Small photos aren't split into tiles, so there is no grid to scale.
  if result and not result.ok and o.grid and result.stderr:find("matches no streams", 1, true) then
    o.grid = false
    result, err = dirfleet.ffmpeg.run({ args = args.ffmpeg(input, output, o) })
  end
  if not result then
    return err
  end
  if not result.ok then
    return last_line(result.stderr)
  end
end

---@param ctx Context
---@param item Target
---@param o PhotoOptions
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
  local run = dirfleet.os == "macos" and with_sips or with_ffmpeg
  local err = run(item.path, output, o)
  if err then
    return { error = err }
  end
  if select and #ctx.targets == 1 then
    dirfleet.app.select(output)
  end
end

M.actions.to_jpeg = {
  run_item = function(ctx, item)
    return convert(ctx, item, { format = "jpeg", quality = "balanced", side = "src" }, "rename", true)
  end,
}

M.actions.convert = {
  ---@param ctx Context
  ---@return Form
  form = function(ctx)
    local single = #ctx.targets == 1
    local first = ctx.targets[1]
    local summary = single and dirfleet.fmt.size(first.size)
      or (#ctx.targets .. " photos, each converted next to the original.")

    local formats = {}
    for _, format in ipairs(args.formats) do
      formats[#formats + 1] = { format.value, format.label }
    end

    local fields = {
      { type = "info", text = summary },
      { id = "format", type = "choice", label = "Format", options = formats, default = "jpeg" },
      {
        id = "quality",
        type = "choice",
        label = "Quality",
        options = { { "small", "Smaller file" }, { "balanced", "Balanced" }, { "best", "Best quality" } },
        default = "balanced",
        visible_if = { field = "format", equals = "jpeg" },
      },
      {
        id = "side",
        type = "choice",
        label = "Longest side",
        options = { { "src", "Same as the source" }, { "4096", "4096 px" }, { "2048", "2048 px" }, { "1080", "1080 px" } },
        default = "src",
        help = "Photos are never enlarged",
      },
      {
        id = "exists",
        type = "choice",
        label = "If the file exists",
        options = { { "rename", "Keep both" }, { "skip", "Skip" }, { "overwrite", "Replace it" } },
        default = "rename",
      },
      { id = "select", type = "bool", label = "Select the result", default = true },
    }
    if dirfleet.os ~= "macos" then
      table.insert(fields, 2, {
        type = "info",
        text = "This format loses the capture date and location here. PNG keeps them.",
        visible_if = { field = "format", not_equals = "png" },
      })
    end
    return {
      title = single and ("Convert " .. first.name) or ("Convert " .. #ctx.targets .. " photos"),
      submit = "Convert",
      remember = true,
      fields = fields,
    }
  end,

  run_item = function(ctx, item, v)
    local o = { format = v.format, quality = v.quality, side = v.side }
    return convert(ctx, item, o, v.exists, v.select)
  end,
}

return M
