local map: { [string]: { name: string, coins: number } }? = nil
local key = "one"
local value = (if map == nil then nil else map[key])
local sure = (if map == nil then (error("map is nil") :: never) else map)[key]
local _1 = (if map == nil then nil else map[key]) local name = (if _1 == nil then nil else _1.name)
local coins = (if map == nil then (error("map is nil") :: never) else map)[key].coins
local deep = (if map == nil then (error("map is nil") :: never) else map)[key].name
print(value, sure, name, coins, deep)
