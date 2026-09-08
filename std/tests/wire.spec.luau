--!nonstrict
-- The wire: a remote's arguments packed into a buffer by their widths.
local Lest = require("@lest")
local describe, it, expect = Lest.describe, Lest.it, Lest.expect
local alloy = require("../alloy")
local wire = alloy.wire

local function roundtrip(layout, args)
	local buf, rest = wire.pack(layout, args, "T")
	return wire.unpack(layout, buf, rest), buf
end

describe("wire", function()
	it("packs every width and reads it back", function()
		local layout = { "u8", "u16", "u32", "i8", "i16", "i32", "f32", "f64", "bool", "str" }
		local args = { 200, 60000, 4000000000, -100, -30000, -2000000000, 1.5, 1 / 3, true, "hello" }
		local out, buf = roundtrip(layout, args)
		for i = 1, 6 do
			expect(out[i]).toBe(args[i])
		end
		expect(out[7]).toBe(1.5)
		expect(out[8]).toBe(1 / 3)
		expect(out[9]).toBe(true)
		expect(out[10]).toBe("hello")
		expect(buffer.len(buf)).toBe(1 + 2 + 4 + 1 + 2 + 4 + 4 + 8 + 1 + 4 + 5)
	end)

	it("keeps an optional nil and passes the rest beside the buffer", function()
		local layout = { "u8?", "any", "str?", "any" }
		local t = { x = 1 }
		local out, buf = roundtrip(layout, { nil, t, "s", 5 })
		expect(out[1]).toBe(nil)
		expect(out[2]).toBe(t)
		expect(out[3]).toBe("s")
		expect(out[4]).toBe(5)
		expect(out.n).toBe(4)
		expect(buffer.len(buf)).toBe(1 + 1 + 4 + 1)
	end)

	it("packs a record, an array, and a struct with its metatable", function()
		local Stats = {}
		Stats.__index = Stats
		function Stats.sum(self)
			return self.level + self.hp
		end
		local layout = {
			{ fields = { { "level", "u8" }, { "hp", "f64" }, { "title", "str?" } }, struct = Stats },
			{ item = "str", array = true },
			{ fields = { { "x", "f32" }, { "y", "f32" } }, optional = true },
			{ item = { fields = { { "id", "u16" }, { "who", "any" } } }, array = true },
		}
		local who = { name = "p" }
		local out, buf = roundtrip(layout, {
			{ level = 3, hp = 10, title = nil },
			{ "a", "bc" },
			nil,
			{ { id = 1, who = who }, { id = 2, who = nil } },
		})
		expect(out[1].level).toBe(3)
		expect(out[1].hp).toBe(10)
		expect(out[1].title).toBe(nil)
		expect(getmetatable(out[1])).toBe(Stats)
		expect(out[1]:sum()).toBe(13)
		expect(out[2][1]).toBe("a")
		expect(out[2][2]).toBe("bc")
		expect(#out[2]).toBe(2)
		expect(out[3]).toBe(nil)
		expect(out[4][1].id).toBe(1)
		expect(out[4][1].who).toBe(who)
		expect(out[4][2].id).toBe(2)
		expect(out[4][2].who).toBe(nil)
		expect(buffer.len(buf)).toBe(1 + 8 + 1 + 4 + (4 + 1) + (4 + 2) + 1 + 4 + 2 + 2)
	end)

	it("names the path of a value that does not fit", function()
		expect(function()
			wire.pack({ { fields = { { "hp", "u8" } } } }, { { hp = 300 } }, "Sync")
		end).toThrow("Sync: argument 1.hp: 300 does not fit @u8")
		expect(function()
			wire.pack({ { item = "u8", array = true } }, { { 1, 2, 999 } }, "Sync")
		end).toThrow("Sync: argument 1[3]: 999 does not fit @u8")
	end)

	it("refuses a value that does not fit", function()
		expect(function()
			wire.pack({ "u8" }, { 300 }, "Heal")
		end).toThrow("Heal: argument 1: 300 does not fit @u8")
		expect(function()
			wire.pack({ "i8" }, { 1.5 }, "Heal")
		end).toThrow("does not fit @i8")
		expect(function()
			wire.pack({ "f64" }, { nil }, "Heal")
		end).toThrow("Heal: argument 1: is nil")
	end)
end)
