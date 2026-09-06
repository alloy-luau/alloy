local lest = require("@lest")
local describe, it, expect = lest.describe, lest.it, lest.expect
local shim = require("../shim")
-- Each chunk has its own globals on this VM, so the doubles bind here.
local typeof, Vector3, Color3, UDim2, Enum, Instance, task, game, workspace =
	shim.typeof, shim.Vector3, shim.Color3, shim.UDim2, shim.Enum, shim.Instance, shim.task, shim.game, shim.workspace

describe("shim", function()
	it("fills the engine globals on a plain VM", function()
		expect(typeof(Vector3.new(1, 2, 3))).toBe("Vector3")
		expect((Vector3.new(1, 2, 3) + Vector3.one).Magnitude > 3).toBe(true)
		expect(Color3.fromRGB(255, 0, 0) == Color3.new(1, 0, 0)).toBe(true)
		expect(UDim2.fromScale(1, 0.5).Y.Scale).toBe(0.5)
		expect(Enum.Font.GothamBold.Name).toBe("GothamBold")
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
end)
