--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local Attributes = alloy.Attributes

describe("Attributes", function()
	local icon = alloy.attribute("icon", { "struct" }, { "asset" })
	local range = alloy.attribute("range", { "field" }, { "min", "max" })

	it("declares a frozen attribute", function()
		expect(icon.name).toBe("icon")
		expect(table.isfrozen(icon)).toBe(true)
	end)

	it("reads a struct's own attributes and its fields'", function()
		local Item = alloy.attrs({}, {
			own = { icon = { "rbxassetid://1" } },
			fields = { count = { range = { 0, 99 } }, name = {} },
		})
		expect(Attributes.get(Item, icon)).toEqual({ "rbxassetid://1" })
		expect(Attributes.get(Item, range)).toBeNil()
		expect(Attributes.field(Item, "count", range)).toEqual({ 0, 99 })
		expect(Attributes.field(Item, "name", range)).toBeNil()
		expect(Attributes.of(Item)).toEqual({ icon = { "rbxassetid://1" } })
		local seen = {}
		for field, data in Attributes.fields(Item, range) do
			seen[field] = data
		end
		expect(seen).toEqual({ count = { 0, 99 } })
	end)

	it("reads a variant's attributes", function()
		local Msg = alloy.attrs({}, { variants = { Quit = { icon = { "x" } } } })
		expect(Attributes.variant(Msg, "Quit", icon)).toEqual({ "x" })
		expect(Attributes.variant(Msg, "Move", icon)).toBeNil()
	end)

	it("attaches to a function value", function()
		local f = alloy.attach(function() end, { icon = { "f" } })
		expect(Attributes.get(f, icon)).toEqual({ "f" })
		expect(Attributes.of(f)).toEqual({ icon = { "f" } })
		expect(Attributes.of(function() end)).toEqual({})
		expect(Attributes.get({}, icon)).toBeNil()
	end)
end)

return nil
