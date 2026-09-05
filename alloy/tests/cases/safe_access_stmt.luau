local function run(handler, part)
	if handler.on_done ~= nil then handler.on_done(1) end
	local _1 = (if handler == nil then nil else handler.child) if _1 ~= nil then _1:Destroy() end
	_1 = (if part == nil then nil else part:FindFirstChild("Fire")) if _1 ~= nil then _1:Play() end
	do (if handler.on_done == nil then error("handler.on_done is nil") else handler.on_done)(2) end
end
