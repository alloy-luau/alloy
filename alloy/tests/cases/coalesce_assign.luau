if x == nil then x = 1 end
if t.a.b == nil then t.a.b = c end
local _1 = f() if t[_1] == nil then t[_1] = x end
_1 = get() if _1[k] == nil then _1[k] = v end
local function f()
	if cache[key] == nil then cache[key] = compute() end
	if cache[key] == nil then cache[key] = other() end
end
