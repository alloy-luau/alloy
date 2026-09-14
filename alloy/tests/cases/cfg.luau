local __alloy = require("@alloy") -- `@cfg` on a function opens the body with the check.

local function save(player: Player)
    if not (__alloy.cfg.server()) then error("`save` is @cfg(server) and cannot run here", 2) end print(player)
end


local function hud() if not ((__alloy.cfg.client() and not __alloy.cfg.studio())) then error("`hud` is @cfg(client and not studio) and cannot run here", 2) end
end

-- `@cfg` on a local reads the value only where it holds.

const store = (if __alloy.cfg.server() then game:GetService("DataStoreService") else nil) :: typeof(game:GetService("DataStoreService"))


local verbose = (if (__alloy.cfg.studio() or __alloy.cfg.test()) then {
    level = 2,
} else nil) :: typeof({ level = 2, })

print(save, hud, store, verbose)
