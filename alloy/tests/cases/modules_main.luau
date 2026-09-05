-- Imports are sugar over require. Paths pass through unchanged.

-- emits: local Inventory = require("./inventory")
local Inventory = require("./inventory")

-- emits: local _m = require("./inventory") local add, total = _m.add, _m.total
local _1 = require("./inventory") local add, total, CAP = _1.add, _1.total, _1.MAX_STACK

-- A `type` specifier next to values emits a type alias. The require stays.
-- emits: local _m = require("./inventory") local add = _m.add type Inventory = _m.Inventory
_1 = require("./inventory") local add_item = _1.add type Inventory = _1.Inventory

-- A type-only import. `erase_type_imports` blanks it in the ship artifact.
_1 = require("./inventory") type Item = _1.Item

-- A default export imports as the module value. Same as `* as`.
local theme = require("./theme")

-- `@alias` comes from .luaurc or .config.luau.
local Signal = require("@packages/signal")

local sword: Item = { id = 1, name = "Sword", count = 1 }
local bag: Inventory = add({}, sword)
add_item(bag, sword)

print(total(bag), CAP, Inventory.version)
print(Signal, theme.primary)
