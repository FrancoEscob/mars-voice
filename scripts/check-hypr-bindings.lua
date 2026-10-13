-- Test shipped callbacks without touching Hyprland, audio, or the filesystem.
local calls, bindings = {}, {}
local seed = 100000
io.popen = function()
    return {read = function() return tostring(seed) end, close = function() return true end}
end
hl = {
    dsp = { exec_cmd = function(command) return command end },
    dispatch = function(command) table.insert(calls, command) end,
    bind = function(key, action, flags)
        bindings[key] = bindings[key] or {}
        table.insert(bindings[key], {action = action, flags = flags or {}})
    end,
    window_rule = function(_) end,
}
local function token(command) return tonumber(command:match("%-%-shortcut%-token (%d+)$")) end
local function is(command, kind) return command:find("--shortcut-" .. kind,1,true) end
local module = arg[1] or "packaging/hyprland.lua.example"
dofile(module)
assert(#calls == 1 and is(calls[1],"release"), "reload must release an old hold")
local press_key = arg[2] or "CTRL + SPACE"
local press = bindings[press_key][1].action
local release = bindings.SPACE[1].action
local ctrl_release = bindings.Control_L[1].action
for _, key in ipairs({"SPACE", "Control_L", "Control_R"}) do
    local f = bindings[key][1].flags
    assert(f.release and f.ignore_mods and f.non_consuming, "release must preserve app input")
end
release()
assert(#calls == 1, "ordinary spaces must not start a process")
press(); press()
assert(#calls == 2 and is(calls[2],"press"), "repeat must not toggle again")
ctrl_release(); release()
assert(#calls == 3 and is(calls[3],"release") and token(calls[2]) == token(calls[3]), "Ctrl first must release the same gesture exactly once")
press(); release(); ctrl_release()
assert(#calls == 5 and is(calls[5],"release") and token(calls[4]) == token(calls[5]), "Space first must release exactly once")
assert(token(calls[4]) > token(calls[2]), "next gesture must be newer")
press()
local held = token(calls[6])
seed = seed + 5000
_G.dicta_shortcut_token = nil -- A complete Lua context reload.
dofile(module)
assert(is(calls[7],"release") and token(calls[7]) > held, "reload must invalidate even a delayed press process")
print("OK: gesture tokens, full reload, repeats, both release orders, ordinary spaces")
if press_key:find("SHIFT", 1, true) then
    for _, key in ipairs({"Shift_L", "Shift_R"}) do
        local binding = assert(bindings[key], "Shift release is required")[1]
        local f = binding.flags
        assert(f.release and f.ignore_mods and f.non_consuming)
        local current_press = bindings[press_key][#bindings[press_key]].action
        local current_release = bindings[key][#bindings[key]].action
        local count = #calls
        current_press(); current_release(); current_release()
        assert(#calls == count + 2 and is(calls[#calls], "release"))
        assert(token(calls[#calls-1]) == token(calls[#calls]))
    end
    print("OK: Shift releases the active gesture exactly once")
end
local read = bindings["CTRL + ALT + SPACE"]
if read then
    local action = read[1].action
    assert(type(action) == "string" and action:find("--read-selection", 1, true), "read bind must call --read-selection")
    assert(not action:find("--shortcut", 1, true), "reading has no hold gesture")
    print("OK: lectura en voz alta con Ctrl+Alt+Space")
end
