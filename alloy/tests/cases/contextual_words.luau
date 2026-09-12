--!strict
-- Ordinary Roblox Luau: each word below is a name here.
local new = Instance.new
local export = {}
local try = pcall
local match = string.match
local const = 1

-- The same words still open their own syntax in the same file.
export type Name = string

const LIMIT = 5

--- Makes an instance by name.
function export.spawn(name: Name): Instance
    return new(name)
end

--- Reads the first word of a string.
function export.pick(s: Name): string?
    return match(s, "%a+")
end

const = const + LIMIT
do local _m1 = const
    if _m1 == 6 then print("six")
    else print(const)
end end
print(try(print), export.spawn, export.pick)
return export
