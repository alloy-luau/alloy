-- A plain Luau module returns one value and has no export table. A
-- bare name binds that value, `* as` binds the same table, and a name
-- in braces reads a field of it.
local util = require("./util")
local Util = require("./util")
local _m1 = require("./util") local helper = _m1.helper

print(util, Util, helper)
