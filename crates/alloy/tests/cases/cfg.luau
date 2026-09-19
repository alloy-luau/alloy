local __alloy = require("@alloy") -- `@cfg` on a function with nothing to give returns where the
-- condition fails.

local function save(player: Player)
    if not (__alloy.cfg.server()) then return end print(player)
end


local function hud() if not ((__alloy.cfg.client() and not __alloy.cfg.studio())) then return end
end

-- One that owes the caller a value raises instead.

local function key(): string
    if not (__alloy.cfg.server()) then error("`key` is @cfg(server) and cannot run here", 2) end return "store"
end

-- `@cfg` on a local reads the value only where it holds.

const store = (if __alloy.cfg.server() then game:GetService("DataStoreService") else nil) :: typeof(game:GetService("DataStoreService"))


local verbose = (if (__alloy.cfg.studio() or __alloy.cfg.test()) then {
    level = 2,
} else nil) :: typeof({ level = 2, })

-- `@cfg` on a statement skips the statement.

if __alloy.cfg.server() then print(store) end


if __alloy.cfg.client() then if verbose then
    print(verbose)
end end

print(save, hud, key, store, verbose)
