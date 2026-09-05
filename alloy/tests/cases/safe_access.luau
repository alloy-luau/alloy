local a = (if t == nil then nil else t.x)
local _1 = (if t == nil then nil else t.x) local b = (if _1 == nil then nil else _1.y.z)
_1 = f() local c = (if _1 == nil then nil else _1.x)
local d = (if t == nil then nil else t[k])
local e = (if t == nil then nil else t:m(1, g()))
local h = (if cb == nil then nil else cb(1))
_1 = (if w == nil then nil else w:FindFirstChild("Map")) local _2 = (if _1 == nil then nil else _1:FindFirstChild("Spawns")) local i = (if _2 == nil then nil else _2:FindFirstChild("Spawn1"))
_1 = (if w == nil then nil else w:FindFirstChild("Spawn Points")) _2 = (if _1 == nil then nil else _1:FindFirstChild(`Slot{n}`)) local j = (if _2 == nil then nil else _2:FindFirstChild(name))
local k = gui:WaitForChild("Hud"):WaitForChild("Health")
local l = (if m.PrimaryPart == nil then error("m.PrimaryPart is nil") else m.PrimaryPart)
_1 = (if t == nil then nil else t.x) local m2 = (if _1 == nil then error("t?.x is nil") else _1).y
_1 = (if w == nil then nil else w:FindFirstChild("Map")) _2 = (if _1 == nil then nil else _1:FindFirstChild("Spawn")) local n = (if _2 == nil then error("w->Map->Spawn is nil") else _2).CFrame
_1 = (if t == nil then nil else t.x) local o = (if _1 == nil then 0 else _1)
local p = ((if t == nil then nil else t.x) :: number).z
if t ~= nil then t.x = 1 end
if t ~= nil then t.x.y += 1 end
if t ~= nil then if t.x == nil then t.x = 2 end end
if t ~= nil then t:m() end
_1 = (if t == nil then nil else t.x) if _1 ~= nil then _1:m() end
do (if cb == nil then error("cb is nil") else cb)(1) end
local function inner(q)
	local _1 = (if q == nil then nil else q.a) return (if _1 == nil then nil else _1.b)
end
