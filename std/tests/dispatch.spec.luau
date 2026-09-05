--!nonstrict
-- Extension methods and `delete`, the two runtime dispatchers.
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")

describe("impl_for and call", function()
	it("routes a call to the extension of the value's kind", function()
		local strings = alloy.impl_for("string")
		function strings.shout(s)
			return string.upper(s) .. "!"
		end
		expect(alloy.call("hi", "shout")).toBe("HI!")
	end)

	it("prefers the receiver's own method", function()
		local obj = setmetatable({}, { __index = { name = function()
			return "own"
		end } })
		expect(alloy.call(obj, "name")).toBe("own")
	end)

	it("static finds a registered static and names a missing one", function()
		local numbers = alloy.impl_for("number")
		function numbers.origin()
			return 0
		end
		expect(alloy.static("number", "origin")()).toBe(0)
		expect(function()
			alloy.static("number", "missing")
		end).toThrow("number.missing is not defined")
	end)
end)

describe("delete", function()
	local function tracker()
		local calls = {}
		local function note(name)
			return function()
				table.insert(calls, name)
			end
		end
		return calls, note
	end

	it("disconnects before it destroys, Roblox spelling first", function()
		local calls, note = tracker()
		alloy.delete({ Disconnect = note("Disconnect"), Destroy = note("Destroy") })
		alloy.delete({ disconnect = note("disconnect"), Destroy = note("Destroy") })
		alloy.delete({ Destroy = note("Destroy"), destroy = note("destroy") })
		alloy.delete({ destroy = note("destroy") })
		expect(calls).toEqual({ "Disconnect", "disconnect", "Destroy", "destroy" })
	end)

	it("passes self to the method", function()
		local target = {}
		local got
		function target:Destroy()
			got = self
		end
		alloy.delete(target)
		expect(got).toBe(target)
	end)

	it("rejects a value with none of the methods", function()
		expect(function()
			alloy.delete({})
		end).toThrow("no Disconnect, disconnect, Destroy, or destroy")
		expect(function()
			alloy.delete(5)
		end).toThrow("cannot delete a number")
	end)
end)

return nil
