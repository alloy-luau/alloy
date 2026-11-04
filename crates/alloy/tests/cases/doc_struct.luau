--- A struct whose fields have doc comments.
local __alloy = require("@alloy") local Counter = {} Counter.__index = Counter export type Counter = typeof(setmetatable(({} :: any) :: {
--- The public count.
read count: number,
--- A private field. This comment must not document a type.
step: number, -- trailing
--- A field under an attribute.
sum: number, total: number,

 }, Counter)) setmetatable(Counter, { __call = function(_, f: { count: number, step: number?, sum: number }): Counter if f.step == nil then local _step: number = 1 f.step = _step end return ((setmetatable(f, Counter) :: any) :: Counter) end }) function Counter.new(f: { count: number, step: number?, sum: number }): Counter return Counter(f) end Counter.__tostring = function(s) return __alloy.show_struct("Counter", s, { "count", "step", "sum" }) end __alloy.attrs(Counter, { own = {}, fields = { sum = { alias = { "total" } } } }) local Counter__alias = { total = "sum" } Counter.__index = function(t, k) local a = Counter__alias[k] if a ~= nil then return rawget(t, a) end return Counter[k] end Counter.__newindex = function(t, k, v) local a = Counter__alias[k] if a ~= nil then rawset(t, a, v) return end rawset(t, k, v) end return { Counter = Counter }
