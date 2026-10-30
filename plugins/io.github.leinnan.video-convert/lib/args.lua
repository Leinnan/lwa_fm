-- FFmpeg arguments for a conversion. Pure: no dirfleet calls, so it is
-- easy to test.

local A = {}

---@class ConvertOptions
---@field format "mp4"|"mkv"|"webm"|"gif"|"mp3"
---@field quality? "fast"|"balanced"|"high"
---@field scale? "src"|"1080"|"720"|"480"
---@field audio? boolean Keep the audio track (default true).
---@field start? number Start of the fragment, in seconds.
---@field finish? number End of the fragment, in seconds.
---@field overwrite? boolean Replace an existing output.

A.formats = {
  { value = "mp4", label = "MP4 (H.264 + AAC)", ext = "mp4", encoder = "libx264" },
  { value = "mkv", label = "MKV (H.264 + AAC)", ext = "mkv", encoder = "libx264" },
  { value = "webm", label = "WebM (VP9 + Opus)", ext = "webm", encoder = "libvpx-vp9" },
  { value = "gif", label = "Animated GIF", ext = "gif", encoder = "gif" },
  { value = "mp3", label = "Audio only (MP3)", ext = "mp3", encoder = "libmp3lame" },
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
  fast = { crf = 28, vp9 = 38, preset = "veryfast" },
  balanced = { crf = 23, vp9 = 32, preset = "medium" },
  high = { crf = 18, vp9 = 24, preset = "slow" },
}

local function seconds(value)
  return string.format("%.3f", value)
end

--- The FFmpeg arguments converting `input` to `output`.
---@param input string
---@param output string
---@param o ConvertOptions
---@return string[]
function A.build(input, output, o)
  local args = {}
  local function add(...)
    for _, value in ipairs({ ... }) do
      args[#args + 1] = tostring(value)
    end
  end
  -- Before -i, -ss seeks quickly and -to is a time in the input.
  if o.start and o.start > 0 then
    add("-ss", seconds(o.start))
  end
  if o.finish then
    add("-to", seconds(o.finish))
  end
  add("-i", input)
  local quality = QUALITY[o.quality or "balanced"] or QUALITY.balanced
  local height = o.scale and o.scale ~= "src" and tonumber(o.scale) or nil
  local audio = o.audio ~= false
  if o.format == "mp3" then
    add("-vn", "-c:a", "libmp3lame", "-q:a", "2")
  elseif o.format == "gif" then
    local scale = height and ("scale=-2:" .. height) or "scale='min(480,iw)':-2"
    add("-vf", "fps=12," .. scale .. ":flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse", "-an")
  else
    if o.format == "webm" then
      add("-c:v", "libvpx-vp9", "-b:v", "0", "-crf", quality.vp9, "-row-mt", "1")
    else
      add("-c:v", "libx264", "-crf", quality.crf, "-preset", quality.preset, "-pix_fmt", "yuv420p")
    end
    if height then
      add("-vf", "scale=-2:" .. height)
    end
    if not audio then
      add("-an")
    elseif o.format == "webm" then
      add("-c:a", "libopus", "-b:a", "128k")
    else
      add("-c:a", "aac", "-b:a", "192k")
    end
    if o.format == "mp4" then
      add("-movflags", "+faststart")
    end
  end
  add(o.overwrite and "-y" or "-n", output)
  return args
end

return A
