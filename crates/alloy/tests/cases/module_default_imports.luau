-- A bare name reads the module's `export default`; `* as` takes the
-- whole table, where the default sits under `default`. The two mix
-- with a named list and with `import type`.
local Theme = require("./theme").default
local _m1 = require("./theme") local Palette = _m1.default local swatch = _m1.swatch
local All = require("./theme")
                                                     

local dark: Shade = All.default

print(Theme, Palette, swatch, All.default, dark)
