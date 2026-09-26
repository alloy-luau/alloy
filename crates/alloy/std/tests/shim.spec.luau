local lest = require("@lest")
local describe, it, expect = lest.describe, lest.it, lest.expect
local shim = require("../shim")
local alloy = require("../alloy")
-- Each chunk has its own globals on this VM, so the doubles bind here.
local typeof, Vector3, Color3, UDim2, Enum, Instance, task, game, workspace =
	shim.typeof, shim.Vector3, shim.Color3, shim.UDim2, shim.Enum, shim.Instance, shim.task, shim.game, shim.workspace

describe("shim", function()
	it("rolls a seeded Random the same way twice", function()
		local Random = shim.Random
		local a, b = Random.new(42), Random.new(42)
		expect(typeof(a)).toBe("Random")
		for _ = 1, 5 do
			expect(a:NextNumber()).toBe(b:NextNumber())
		end
		for _ = 1, 50 do
			local n = a:NextInteger(3, 5)
			expect(n >= 3 and n <= 5 and n == math.floor(n)).toBe(true)
			local x = a:NextNumber(10, 20)
			expect(x >= 10 and x < 20).toBe(true)
		end
		local c = a:Clone()
		expect(c:NextNumber()).toBe(a:NextNumber())
	end)

	it("fills the engine globals on a plain VM", function()
		expect(typeof(Vector3.new(1, 2, 3))).toBe("Vector3")
		expect((Vector3.new(1, 2, 3) + Vector3.one).Magnitude > 3).toBe(true)
		expect(Color3.fromRGB(255, 0, 0) == Color3.new(1, 0, 0)).toBe(true)
		expect(UDim2.fromScale(1, 0.5).Y.Scale).toBe(0.5)
		expect(Enum.Font.GothamBold.Name).toBe("GothamBold")
	end)

	it("turns a CFrame with the engine's math", function()
		local CFrame = shim.CFrame
		local function near(v, x, y, z)
			expect(v.X).toBeCloseTo(x, 6)
			expect(v.Y).toBeCloseTo(y, 6)
			expect(v.Z).toBeCloseTo(z, 6)
		end

		-- lookAt faces the target, with the UpVector toward +Y.
		local face = CFrame.lookAt(Vector3.new(1, 2, 3), Vector3.new(6, 2, 3))
		expect(typeof(face)).toBe("CFrame")
		near(face.Position, 1, 2, 3)
		near(face.LookVector, 1, 0, 0)
		near(face.RightVector, 0, 0, 1)
		near(face.UpVector, 0, 1, 0)
		near(face.ZVector, -1, 0, 0)
		near(CFrame.lookAt(Vector3.zero, Vector3.new(0, 0, 5)).RightVector, -1, 0, 0)
		near(CFrame.lookAt(Vector3.zero, Vector3.new(0, 9, 0)).LookVector, 0, 1, 0)
		expect(CFrame.new(Vector3.new(1, 2, 3), Vector3.new(6, 2, 3))).toEqual(face)

		-- Angles turns about Z, then Y, then X, as the engine does.
		near(CFrame.Angles(0, math.rad(90), 0).LookVector, -1, 0, 0)
		near(CFrame.Angles(math.rad(90), 0, 0).LookVector, 0, 1, 0)
		expect(CFrame.fromAxisAngle(Vector3.yAxis, math.rad(90)):FuzzyEq(CFrame.Angles(0, math.rad(90), 0))).toBe(true)
		local rx, ry, rz = CFrame.Angles(0.3, -0.4, 1.1):ToEulerAnglesXYZ()
		expect(rx).toBeCloseTo(0.3, 6)
		expect(ry).toBeCloseTo(-0.4, 6)
		expect(rz).toBeCloseTo(1.1, 6)
		rx, ry, rz = CFrame.fromOrientation(0.3, -0.4, 1.1):ToOrientation()
		expect(rx).toBeCloseTo(0.3, 6)
		expect(ry).toBeCloseTo(-0.4, 6)
		expect(rz).toBeCloseTo(1.1, 6)

		-- A CFrame times a Vector3 is a point in world space.
		local turned = CFrame.new(1, 0, 0) * CFrame.Angles(0, math.rad(90), 0)
		local point = turned * Vector3.new(0, 0, -1)
		expect(typeof(point)).toBe("Vector3")
		near(point, 0, 0, 0)
		near(turned:VectorToObjectSpace(turned:VectorToWorldSpace(Vector3.new(1, 2, 3))), 1, 2, 3)
		local c = CFrame.lookAt(Vector3.new(3, 1, 2), Vector3.new(-4, 5, 9))
		near(c:PointToObjectSpace(c:PointToWorldSpace(Vector3.new(1, 2, 3))), 1, 2, 3)
		expect((c * c:Inverse()):FuzzyEq(CFrame.identity)).toBe(true)
		expect(c:ToWorldSpace(c:ToObjectSpace(turned)):FuzzyEq(turned)).toBe(true)
		near(c.Rotation.Position, 0, 0, 0)
		near(c.Rotation.LookVector, c.LookVector.X, c.LookVector.Y, c.LookVector.Z)
		near((c + Vector3.new(1, 0, 0)).Position, 4, 1, 2)

		-- Lerp moves in a line and turns on the shortest arc.
		local half = CFrame.new():Lerp(CFrame.Angles(0, math.rad(90), 0) + Vector3.new(2, 0, 0), 0.5)
		near(half.Position, 1, 0, 0)
		near(half.LookVector, -math.sqrt(0.5), 0, -math.sqrt(0.5))

		-- The components round trip, and a quaternion builds a rotation.
		expect(CFrame.new(c:GetComponents())).toEqual(c)
		expect(CFrame.new(1, 2, 3, 0, 0, 0, 1) == CFrame.new(1, 2, 3)).toBe(true)
		near(CFrame.new(0, 0, 0, 0, math.sqrt(0.5), 0, math.sqrt(0.5)).LookVector, -1, 0, 0)
		near(Vector3.new(3, 0, 4).Unit, 0.6, 0, 0.8)
	end)

	it("keeps a small instance tree", function()
		local part = Instance.new("Part")
		part.Name = "P"
		part.Parent = workspace
		expect(workspace:FindFirstChild("P")).toBe(part)
		expect(part:IsA("BasePart")).toBe(true)
		expect(typeof(part)).toBe("Instance")
		local seen = nil
		part.Changed:Connect(function(key)
			seen = key
		end)
		part.Anchored = true
		expect(seen).toBe("Anchored")
		part:SetAttribute("Zone", "Sky")
		expect(part:GetAttribute("Zone")).toBe("Sky")
		part:Destroy()
		expect(workspace:FindFirstChild("P")).toBe(nil)
		expect(game:GetService("Players"):GetPlayers()).toEqual({})
	end)

	it("parks a waiting thread until a step", function()
		local count = 0
		task.spawn(function()
			while true do
				task.wait(1)
				count += 1
			end
		end)
		expect(count).toBe(0)
		expect(shim.step()).toBe(1)
		expect(count).toBe(1)
	end)

	-- An async test runs on a thread that cannot yield; the spec hands
	-- its Future to `settle`, which steps the parked threads.
	it("settles a Future whose thread a wait parked", function()
		local f = alloy.future(function()
			task.wait()
			task.wait(1)
			return 5
		end)
		expect(f:is_settled()).toBe(false)
		expect(alloy.await(shim.settle(f))).toBe(5)
	end)

	it("fails a Future that no parked thread settles", function()
		local f = alloy.future(function()
			coroutine.yield()
		end)
		expect(function()
			shim.settle(f)
		end).toThrow("the test awaits a Future that no parked thread settles")
	end)
end)
