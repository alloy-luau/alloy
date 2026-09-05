--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local Scope, Signal = alloy.Scope, alloy.Signal

describe("Scope", function()
	it("cleans newest first and empties itself", function()
		local order = {}
		local scope = Scope.new()
		scope:add(function()
			table.insert(order, "first")
		end)
		local thing = scope:add({
			Destroy = function()
				table.insert(order, "second")
			end,
		})
		expect(thing.Destroy).toBeTruthy()
		expect(scope:len()).toBe(2)
		scope:clean()
		expect(order).toEqual({ "second", "first" })
		expect(scope:len()).toBe(0)
		scope:clean()
		expect(order).toEqual({ "second", "first" })
	end)

	it("disconnects a signal connection", function()
		local sig = Signal.new()
		local fired = 0
		local scope = Scope.new()
		scope:add(sig:Connect(function()
			fired += 1
		end))
		sig:Fire()
		scope:clean()
		sig:Fire()
		expect(fired).toBe(1)
	end)

	it("removes an item without cleaning it, and nests", function()
		local ran = false
		local scope = Scope.new()
		local f = scope:add(function()
			ran = true
		end)
		expect(scope:remove(f)).toBe(true)
		expect(scope:remove(f)).toBe(false)
		local inner = scope:extend()
		local inner_ran = false
		inner:add(function()
			inner_ran = true
		end)
		alloy.delete(scope)
		expect(ran).toBe(false)
		expect(inner_ran).toBe(true)
	end)
end)
