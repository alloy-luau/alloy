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

	it("keeps its order when the ring wraps and grows", function()
		local q = Queue.new()
		local popped = {}
		local next_in = 1
		-- Pops keep the front moving, so pushes wrap past the end of the
		-- ring, and the queue grows while it has wrapped.
		for round = 1, 60 do
			for _ = 1, round % 7 + 1 do
				q:push(next_in)
				next_in += 1
			end
			for _ = 1, round % 5 do
				local v = q:pop()
				if v ~= nil then
					table.insert(popped, v)
				end
			end
			local front = (popped[#popped] or 0) + 1
			local arr = q:to_array()
			expect(arr:len()).toBe(q:len())
			for i, v in arr do
				expect(v).toBe(front + i - 1)
			end
			local k = 0
			for v in q do
				k += 1
				expect(v).toBe(arr[k])
			end
			expect(k).toBe(q:len())
		end
		while not q:is_empty() do
			table.insert(popped, q:pop())
		end
		expect(#popped).toBe(next_in - 1)
		for i, v in popped do
			expect(v).toBe(i)
		end
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

	it("to_table gives the live ring, keyed from the front", function()
		local q = Queue.from({ 1, 2 })
		expect(q:to_table()).toEqual({ 1, 2 })
		q:pop()
		-- A pop moves the front up, so the keys no longer start at 1.
		expect(q:to_table()[1]).toBeNil()
		expect(q:to_table()[2]).toBe(2)
		q:to_table()[2] = 9
		expect(q:peek()).toBe(9)
	end)
end)
