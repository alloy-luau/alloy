-- Imports are sugar over require. Paths pass through unchanged.

-- emits: local Inventory = require("./inventory")
local Inventory = require("./inventory")

-- emits: local _m = require("./inventory") local add, total = _m.add, _m.total
local _m1 = require("./inventory") local add, total, CAP = _m1.add, _m1.total, _m1.MAX_STACK

-- A `type` specifier next to values emits a type alias. The require stays.
-- emits: local _m = require("./inventory") local add = _m.add type Inventory = _m.Inventory
local _m2 = require("./inventory") local add_item = _m2.add type Inventory = _m2.Inventory

-- A type-only import. `erase_type_imports` blanks it in the ship artifact.
local _m3 = require("./inventory") type Item = _m3.Item

-- A bare name takes the module's `export default`, under any name.
local theme = require("./theme").default

-- `@alias` comes from .luaurc or .config.luau.
local Signal = require("@packages/signal")

local sword: Item = { id = 1, name = "Sword", count = 1 }
local bag: Inventory = add({}, sword)
add_item(bag, sword)

print(total(bag), CAP, Inventory.version)
print(Signal, theme.primary)
