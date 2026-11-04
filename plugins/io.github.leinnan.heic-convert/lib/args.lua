-- Arguments for a conversion, for sips and FFmpeg. Pure: no dirfleet calls,
-- so it is easy to test.

local A = {}

---@class PhotoOptions
---@field format "jpeg"|"png"|"tiff"
---@field quality? "small"|"balanced"|"best" JPEG only.
---@field side? string|integer Longest side in pixels; "src" keeps the size.
---@field grid? boolean FFmpeg: the photo is a tile grid.
---@field overwrite? boolean FFmpeg: replace an existing output.

A.formats = {
  { value = "jpeg", label = "JPEG", ext = "jpg" },
  { value = "png", label = "PNG (lossless)", ext = "png" },
  { value = "tiff", label = "TIFF (lossless)", ext = "tiff" },
}

---@return table?
function A.format(value)
  for _, format in ipairs(A.formats) do
    if format.value == value then
      return format
    end
  end
end

local QUALITY = {
  small = { ffmpeg = 7, sips = 70 },
  balanced = { ffmpeg = 4, sips = 85 },
  best = { ffmpeg = 2, sips = 95 },
}

local function quality(o)
  return QUALITY[o.quality or "balanced"] or QUALITY.balanced
end

--- Whether this FFmpeg joins a photo's tiles. Before 8.1 the command line
--- writes one tile as if it were the photo. Builds without a version number
--- (from git) get the benefit of the doubt.
---@param line string? The first line of `ffmpeg -version`.
---@return boolean
function A.ffmpeg_supported(line)
  local major, minor = (line or ""):match("^ffmpeg version n?(%d+)%.(%d+)")
  if not major then
    return true
  end
  major, minor = tonumber(major), tonumber(minor)
  return major > 8 or (major == 8 and minor >= 1)
end

--- The FFmpeg arguments converting `input` to `output`.
---@param input string
---@param output string
---@param o PhotoOptions
---@return string[]
function A.ffmpeg(input, output, o)
  local args = { "-i", input }
  local function add(...)
    for _, value in ipairs({ ... }) do
      args[#args + 1] = tostring(value)
    end
  end
  local side = tonumber(o.side)
  if side then
    local scale = string.format(
      "scale='min(iw,%d)':'min(ih,%d)':force_original_aspect_ratio=decrease:flags=lanczos",
      side,
      side
    )
    -- FFmpeg joins tiles in a filter graph of its own, which -vf can't extend.
    if o.grid then
      add("-filter_complex", "[0:g:0]" .. scale)
    else
      add("-vf", scale)
    end
  end
  -- -update takes the name literally: no "%d" patterns.
  add("-frames:v", 1, "-update", 1)
  if o.format == "jpeg" then
    add("-c:v", "mjpeg", "-q:v", quality(o).ffmpeg)
  elseif o.format == "tiff" then
    -- FFmpeg would keep the photo's YCbCr, which many programs can't read.
    add("-pix_fmt", "rgb24")
  end
  add(o.overwrite and "-y" or "-n", output)
  return args
end

--- The sips arguments converting `input` to `output`. sips enlarges to -Z,
--- so `o.side` must be smaller than the photo.
---@param input string
---@param output string
---@param o PhotoOptions
---@return string[]
function A.sips(input, output, o)
  local args = { "-s", "format", o.format }
  local function add(...)
    for _, value in ipairs({ ... }) do
      args[#args + 1] = tostring(value)
    end
  end
  if o.format == "jpeg" then
    add("-s", "formatOptions", quality(o).sips)
  end
  local side = tonumber(o.side)
  if side then
    add("-Z", side)
  end
  add(input, "--out", output)
  return args
end

return A
