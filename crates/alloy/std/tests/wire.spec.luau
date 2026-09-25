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
		-- A string's length is a varint: one byte under 128.
		expect(buffer.len(buf)).toBe(1 + 2 + 4 + 1 + 2 + 4 + 4 + 8 + 1 + 1 + 5)
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
		expect(buffer.len(buf)).toBe(1 + 1 + 1 + 1)
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
		expect(buffer.len(buf)).toBe(1 + 8 + 1 + 1 + (1 + 1) + (1 + 2) + 1 + 1 + 2 + 2)
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

	it("refuses a forged packet instead of trusting it", function()
		-- An array whose count claims more items than the packet holds.
		local forged = buffer.create(5)
		buffer.writeu8(forged, 0, 0xFF)
		buffer.writeu8(forged, 1, 0xFF)
		buffer.writeu8(forged, 2, 0xFF)
		buffer.writeu8(forged, 3, 0x7F)
		expect(function()
			wire.unpack({ { item = "any", array = true } }, forged, { n = 0 })
		end).toThrow("count runs past the packet")
		-- A string longer than what is left.
		local short = buffer.create(2)
		buffer.writeu8(short, 0, 50)
		expect(function()
			wire.unpack({ "str" }, short, { n = 0 })
		end).toThrow("string runs past the packet")
	end)

	it("checks and restores a payload that crossed as a table", function()
		local Stats = {}
		Stats.__index = Stats
		local layout = {
			{ fields = { { "level", "u8" }, { "hp", "f64" } }, struct = Stats },
			{ item = "u8", array = true },
		}
		local out = wire.restore(layout, { n = 2, { level = 3, hp = 10, extra = "dropped" }, { 1, 2 } })
		expect(getmetatable(out[1])).toBe(Stats)
		expect(out[1].extra).toBe(nil)
		expect(#out[2]).toBe(2)
		expect(function()
			wire.restore(layout, { n = 2, { level = 300, hp = 1 }, {} })
		end).toThrow("does not fit @u8")
		expect(function()
			wire.check({ "str" }, { 5 }, "Chat")
		end).toThrow("Chat: argument 1: 5 is not a string")
	end)

	it("gives a payload enum its metatable back and refuses a forged tag", function()
		local Boost = {}
		Boost.__index = Boost
		Boost.None = "None"
		function Boost.Strength(n)
			return setmetatable({ tag = "Strength", _1 = n }, Boost)
		end
		function Boost.describe(self)
			return self.tag
		end
		local node = { enum = Boost, tags = { None = 0, Strength = 1 } }
		-- Roblox copies the table and drops the metatable.
		local out = wire.restore({ node, node }, { n = 2, { tag = "Strength", _1 = 3 }, "None" })
		expect(getmetatable(out[1])).toBe(Boost)
		expect(out[1]:describe()).toBe("Strength")
		expect(out[2]).toBe("None")
		-- Beside a packed buffer, the value takes the same road.
		local packed = roundtrip({ "u8", node }, { 7, { tag = "Strength", _1 = 1 } })
		expect(getmetatable(packed[2])).toBe(Boost)
		expect(function()
			wire.restore({ node }, { n = 1, { tag = "describe" } })
		end).toThrow("is no variant of the enum")
		expect(function()
			wire.restore({ node }, { n = 1, "Strength" })
		end).toThrow("Strength is no variant of the enum")
		expect(wire.restore({ { enum = Boost, tags = { None = 0 }, optional = true } }, { n = 1 })[1]).toBe(nil)
	end)

	it("writes a long string's length in more bytes", function()
		local text = string.rep("x", 300)
		local out, buf = roundtrip({ "str" }, { text })
		expect(out[1]).toBe(text)
		expect(buffer.len(buf)).toBe(2 + 300)
	end)
end)
