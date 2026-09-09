-- The three import forms over a module that returns its value. A bare
-- name binds the value, `* as` binds the same value, and a name in
-- braces reads one key of it.
local Palette = require("./returning_module")
local All = require("./returning_module")
local _m1 = require("./returning_module") local tint = _m1.tint

print(Palette, All, tint)
