local __alloy = require("@alloy") -- A statement under a `return`, a `break`, or a `continue` never runs.
-- Luau takes those three only as the last statement of a block, so the
-- emit fences each one in `do ... end`.
local function scan(xs: __alloy.Array<number>): number
    local found = 0

    for _, x in xs do
        if x > 3 then
            found = x
            do break end
            found = 0
        end
    end

    while found < 10 do
        do continue end
        found += 1
    end

    do return found end
    found = 1
end

return scan
