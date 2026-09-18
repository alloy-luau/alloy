--!nonstrict
local lest = require("@lest")
local describe, it, expect = lest.describe, lest.it, lest.expect
local shim = require("../shim")
local alloy = require("../alloy")
local Scope, Signal = alloy.Scope, alloy.Signal
-- Each chunk has its own globals on this VM, so the doubles bind here.
local Instance, task, game, step = shim.Instance, shim.task, shim.game, shim.step

describe("delete", function()
	it("disconnects a Roblox connection", function()
		local part = Instance.new("Part")
		local seen = 0
		local connection = part.Changed:Connect(function()
			seen += 1
		end)
		part.Anchored = true
		alloy.delete(connection)
		part.Name = "Two"
		expect(seen).toBe(1)
	end)

	it("cancels a thread", function()
		local thread = coroutine.create(function()
			coroutine.yield()
		end)
		coroutine.resume(thread)
		alloy.delete(thread)
		expect(coroutine.status(thread)).toBe("dead")
	end)

	it("disconnects a Signal connection", function()
		local signal = Signal.new()
		local fired = 0
		local connection = signal:Connect(function()
			fired += 1
		end)
		signal:Fire()
		alloy.delete(connection)
		signal:Fire()
		expect(fired).toBe(1)
		expect(connection.Connected).toBe(false)
	end)

	it("closes a Scope", function()
		local ran = false
		local scope = Scope.new()
		scope:add(function()
			ran = true
		end)
		alloy.delete(scope)
		expect(ran).toBe(true)
		expect(scope:len()).toBe(0)
	end)

	it("destroys a Signal", function()
		local signal = Signal.new()
		local fired = 0
		signal:Connect(function()
			fired += 1
		end)
		alloy.delete(signal)
		signal:Fire()
		expect(fired).toBe(0)
	end)

	it("drops what a scope holds for an Instance before the Destroy", function()
		local part = Instance.new("Part")
		local scope = Scope.new()
		local signal = Signal.new()
		local fired = 0
		-- The second argument names the Instance the item belongs to.
		scope:add(
			signal:Connect(function()
				fired += 1
			end),
			part
		)
		expect(scope:len()).toBe(1)
		alloy.delete(part)
		expect(scope:len()).toBe(0)
		signal:Fire()
		expect(fired).toBe(0)
		expect(part.Parent).toBe(nil)
	end)

	it("leaves an item the scope already gave up", function()
		local part = Instance.new("Part")
		local scope = Scope.new()
		local ran = false
		local item = scope:add(function()
			ran = true
		end, part)
		expect(scope:remove(item)).toBe(true)
		alloy.delete(part)
		expect(ran).toBe(false)
	end)
end)

describe("destroy", function()
	it("calls Destroy on an Instance", function()
		local part = Instance.new("Part")
		part.Parent = game
		alloy.destroy(part)
		expect(part.Parent).toBe(nil)
	end)

	it("takes either spelling, Roblox first", function()
		local order = {}
		alloy.destroy({
			Destroy = function()
				table.insert(order, "Destroy")
			end,
			destroy = function()
				table.insert(order, "destroy")
			end,
		})
		alloy.destroy({
			destroy = function()
				table.insert(order, "lower")
			end,
		})
		expect(order).toEqual({ "Destroy", "lower" })
	end)

	it("calls the method and nothing else", function()
		local hit = { disconnect = 0, destroy = 0 }
		alloy.destroy({
			Disconnect = function()
				hit.disconnect += 1
			end,
			destroy = function()
				hit.destroy += 1
			end,
		})
		expect(hit).toEqual({ disconnect = 0, destroy = 1 })
	end)

	it("refuses a value with no method, and a number", function()
		expect(function()
			alloy.destroy({} :: any)
		end).toThrow("no Destroy or destroy method")
		expect(function()
			alloy.destroy(3 :: any)
		end).toThrow("cannot destroy a number")
	end)
end)

describe("destroy after", function()
	it("waits the delay, then destroys", function()
		local left = 5
		alloy.destroy_after({
			destroy = function()
				left = 0
			end,
		}, 0)
		expect(left).toBe(0)
	end)

	it("hands an Instance to Debris", function()
		local part = Instance.new("Part")
		part.Parent = game
		game:GetService("Debris"):AddItem(part, 0)
		expect(part.Parent).toBe(game)
		step()
		expect(part.Parent).toBe(nil)
	end)
end)

-- `after n do ... end` and `after n where c do ... end` lower to these
-- two shapes. The spec runs them, so a change in the emit that breaks
-- the runtime shows here.
describe("after", function()
	it("runs the block when the timer fires", function()
		local ran = false
		task.delay(0, function()
			ran = true
		end)
		expect(ran).toBe(false)
		step()
		expect(ran).toBe(true)
	end)

	it("reads a where condition at the fire, not at the schedule", function()
		local ready = false
		local ran = 0
		task.delay(0, function()
			if ready then
				ran += 1
			end
		end)
		ready = true
		step()
		expect(ran).toBe(1)
	end)

	it("skips the block when the condition is false at the fire", function()
		local ready = true
		local ran = 0
		task.delay(0, function()
			if ready then
				ran += 1
			end
		end)
		ready = false
		step()
		expect(ran).toBe(0)
	end)
end)
