--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")

describe("deferred", function()
	it("runs the blocks last to first after the body", function()
		local order = {}
		local defers = {}
		local a, b = alloy.deferred(defers, function(x)
			defers[#defers + 1] = function()
				table.insert(order, "first")
			end
			defers[#defers + 1] = function()
				table.insert(order, "second")
			end
			table.insert(order, "body")
			return x, nil
		end, 7)
		expect(a).toBe(7)
		expect(b).toBe(nil)
		expect(table.concat(order, " ")).toBe("body second first")
	end)

	it("runs the blocks when the body raises, and passes the error on", function()
		local ran = false
		local defers = {}
		local ok, failure = pcall(alloy.deferred, defers, function()
			defers[1] = function()
				ran = true
			end
			error({ code = 4 })
		end)
		expect(ok).toBe(false)
		expect(failure.code).toBe(4)
		expect(ran).toBe(true)
	end)

	it("runs every block after one raises, and raises that error", function()
		local ran = false
		local defers = {}
		local ok, failure = pcall(alloy.deferred, defers, function()
			defers[1] = function()
				ran = true
			end
			defers[2] = function()
				error("late", 0)
			end
			return 1
		end)
		expect(ok).toBe(false)
		expect(failure).toBe("late")
		expect(ran).toBe(true)
	end)
end)

describe("drop", function()
	it("calls the cleanup a value has", function()
		local seen = {}
		alloy.drop(function()
			table.insert(seen, "function")
		end)
		alloy.drop({
			drop = function()
				table.insert(seen, "drop")
			end,
			Destroy = function()
				table.insert(seen, "never")
			end,
		})
		alloy.drop({
			Disconnect = function(self)
				table.insert(seen, self.tag)
			end,
			tag = "disconnect",
		})
		alloy.drop({ close = function()
			table.insert(seen, "close")
		end })
		expect(table.concat(seen, " ")).toBe("function drop disconnect close")
	end)

	it("leaves a value with no cleanup", function()
		alloy.drop(nil)
		alloy.drop(5)
		alloy.drop({ name = "plain" })
		local thread = coroutine.create(function() end)
		alloy.drop(thread)
		expect(coroutine.status(thread)).toBe("dead")
	end)
end)
