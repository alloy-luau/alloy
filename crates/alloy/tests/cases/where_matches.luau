-- `$matches` and a macro in `where` read the bound value, not the name before it binds.
local __alloy = require("@alloy") local items: __alloy.Array<__alloy.Result<number, string>> = __alloy.Array.from({__alloy.Ok(1), __alloy.Ok(2), __alloy.Err("stop")})
local i = 0
local function next_item(): __alloy.Result<number, string>?
    i += 1
    return items[i]
end



local runs = 0
while true do local _c1 = next_item() if not (_c1 and (( if type(_c1) == "table" and _c1.tag == "Ok" then true else false ))) then break end local x = _c1
    runs += 1
end

do local _c1 = next_item() local _c2 = if _c1 and (( if type(_c1) == "table" and _c1.tag == "Ok" then true else false )) then next_item() else nil if _c1 and (( if type(_c1) == "table" and _c1.tag == "Ok" then true else false )) and _c2 and (( if type(_c2) == "table" and _c2.tag == "Ok" then true else false )) then local a, b = _c1, _c2
    print(a, b)
end end
print(runs) return nil
