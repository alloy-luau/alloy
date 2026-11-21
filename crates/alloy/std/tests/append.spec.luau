local lest = require("@lest")
local describe, it, expect = lest.describe, lest.it, lest.expect
local alloy = require("../alloy")

describe("append", function()
	it("adds to the end of a plain table", function()
		local xs = { 1, 2 }
		alloy.append(xs, 3)
		expect(xs[3]).toBe(3)
		expect(#xs).toBe(3)
	end)

	it("pushes onto an Array and a Queue", function()
		local xs = alloy.Array.from({ 1 })
		alloy.append(xs, 2)
		expect(xs:len()).toBe(2)
		expect(xs:last()).toBe(2)

		local q = alloy.Queue.new()
		alloy.append(q, "a")
		expect(q:pop()).toBe("a")
	end)

	it("adds to a Set once", function()
		local seen = alloy.Set.new()
		alloy.append(seen, "a")
		alloy.append(seen, "a")
		expect(seen:has("a")).toBe(true)
		expect(seen:len()).toBe(1)
	end)

	it("calls the push of a table that has no metatable", function()
		local got = {}
		local stack = {
			push = function(_, v)
				table.insert(got, v)
			end,
		}
		alloy.append(stack, 7)
		expect(got[1]).toBe(7)
	end)
end)

describe("table_call", function()
	it("calls the library on a plain table", function()
		local xs = { 3, 1, 2 }
		alloy.table_call(xs, "insert", 4)
		alloy.table_call(xs, "sort")
		expect(alloy.table_call(xs, "concat", ",")).toBe("1,2,3,4")
		expect(alloy.table_call(xs, "remove")).toBe(4)
		expect(alloy.table_call(xs, "find", 2)).toBe(2)
	end)

	it("calls the method the value has first", function()
		local xs = alloy.Array.from({ 1, 2, 3 })
		-- `Array.find` takes a test, `table.find` a value.
		local found = alloy.table_call(xs, "find", function(v)
			return v > 1
		end)
		expect(found).toBe(2)
		-- An Array has no `insert`, so the library answers.
		alloy.table_call(xs, "insert", 4)
		expect(xs:len()).toBe(4)

		local module = {}
		function module.insert(_, v)
			return "own " .. v
		end
		expect(alloy.table_call(module, "insert", "x")).toBe("own x")
	end)
end)
