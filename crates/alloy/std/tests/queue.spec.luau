--!nonstrict
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local Queue = alloy.Queue

describe("Queue", function()
	it("pops in the order pushed", function()
		local q = Queue.new()
		q:push(1)
		q:push(2)
		q:push(3)
		expect(q:len()).toBe(3)
		expect(q:peek()).toBe(1)
		expect(q:pop()).toBe(1)
		expect(q:pop()).toBe(2)
		expect(q:len()).toBe(1)
		expect(q:pop()).toBe(3)
		expect(q:pop()).toBeNil()
		expect(q:is_empty()).toBe(true)
	end)

	it("starts its indices over when empty", function()
		local q = Queue.from({ "a", "b" })
		q:pop()
		q:pop()
		q:push("c")
		expect(q._head).toBe(1)
		expect(q:pop()).toBe("c")
	end)

	it("iterates front to back without a pop", function()
		local q = Queue.from({ 1, 2, 3 })
		local seen = {}
		for v in q do
			table.insert(seen, v)
		end
		expect(seen).toEqual({ 1, 2, 3 })
		expect(q:len()).toBe(3)
		expect(q:to_array():map(function(v)
			return v * 2
		end)).toEqual({ 2, 4, 6 })
		q:clear()
		expect(q:len()).toBe(0)
	end)
end)
