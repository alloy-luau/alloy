x = if x == nil then 1 else x
if t.a.b == nil then t.a.b = c end
local _1 = g() if t[_1] == nil then t[_1] = x end
_1 = get() if _1[k] == nil then _1[k] = v end
local function f()
	if cache[key] == nil then cache[key] = compute() end
	if cache[key] == nil then cache[key] = other() end
end
