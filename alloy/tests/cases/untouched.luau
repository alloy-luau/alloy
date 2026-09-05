--!strict
-- A file with no Alloy syntax comes back byte for byte.
local Players = game:GetService("Players")

local function greet(p: Player): string
	return `hi {p.Name}` -- keep this comment
end

return { greet = greet }
