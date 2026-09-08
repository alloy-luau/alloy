local function eq(x: number?): number
	x = if x == nil then 1 else x if x == nil then end
	return x
end
local function falsy(y: string?): string
	y = if not y then "z" else y if y == nil then end
	return y
end
local function branch(z: number?): number
	z = if z == nil then 3 else z if z == nil then else print(z) end
	return z
end
local function spread(w: number?): number
	w = if w == nil then
		4 else w if w == nil then
	end
	return w
end
local function two(v: number?): number
	if v == nil then v = 5 print("set") end
	return (if v == nil then 6 else v)
end
print(eq(nil), falsy(nil), branch(nil), spread(nil), two(nil))
