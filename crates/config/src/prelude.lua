-- The `schemify` API surface (config spec §3), compiled once per state before the sandbox
-- strips `debug`: it keeps `debug.getinfo` as a private upvalue, so errors and notes name the
-- calling line the way `luaL_error` does in C Lua. `raw` holds the Rust functions; each
-- returns nil (or its value) on success and an error string on failure, and the wrappers
-- below raise that error at the caller (`error(msg, 2)`). Nothing here is reachable by the
-- config except through the globals it sets.
local raw, getinfo = ...
local error, setmetatable, select, type, pcall_native, tostring =
  error, setmetatable, select, type, pcall, tostring
local concat = table.concat

-- `file:line: ` of the code that called the API function (stack level 3: here = 1, the
-- wrapper = 2).
local function where()
  local i = getinfo(3, "Sl")
  if i and i.currentline and i.currentline > 0 then
    return i.short_src .. ":" .. i.currentline .. ": "
  end
  return ""
end

local function check(err)
  if err then error(err, 3) end
end

local schemify = {}
schemify.opt = setmetatable({}, {
  __index = function(_, k)
    local v, err = raw.get_opt(k)
    if err then error(err, 2) end
    return v
  end,
  __newindex = function(_, k, v)
    local err = raw.set_opt(k, v)
    if err then error(err, 2) end
  end,
})
schemify.keymap = {
  set = function(chord, target, opts) check(raw.keymap_set(chord, target, opts)) end,
}
schemify.cmd = function(name, args) check(raw.cmd(name, args)) end
schemify.on = function(event, f) check(raw.on(event, f)) end
schemify.sim = function(name, spec) check(raw.sim(name, spec)) end
schemify.notify = function(msg, level) check(raw.notify(msg, level)) end
schemify.pack = {
  add = function(specs) check(raw.pack_add(where(), specs)) end,
}

-- `print(...)`: an info notification (a config has no stdout).
print = function(...)
  local parts = {}
  for i = 1, select("#", ...) do parts[i] = tostring((select(i, ...))) end
  check(raw.notify(concat(parts, "\t")))
end

pcall = raw.pcall
xpcall = raw.xpcall

-- `require "a.b"`: `<root>/lua/a/b.lua` or `<root>/lua/a/b/init.lua`, once; a cycle fails
-- instead of recursing.
local loaded, loading = {}, {}
require = function(name)
  local v = loaded[name]
  if v == loading then error("loop requiring `" .. tostring(name) .. "`", 2) end
  if v ~= nil then return v end
  local chunk, err = raw.find_module(name)
  if not chunk then error(err, 2) end
  loaded[name] = loading
  -- The native pcall re-raises a tripped budget or memory cap: they stay uncatchable here.
  local ok, res = pcall_native(chunk, name)
  if not ok then
    loaded[name] = nil
    error(res, 0)
  end
  if res == nil then res = true end
  loaded[name] = res
  return res
end

_G.schemify = schemify
