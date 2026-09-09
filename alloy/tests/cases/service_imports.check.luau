-- A Roblox service is an import: `game:Players` names one, and `game`
-- names every one, so its names go in braces. Both lower to
-- `game:GetService` on the import's own line.
local Players = game:GetService("Players")
local ReplicatedStorage = game:GetService("ReplicatedStorage") local Run = game:GetService("RunService")

local remotes = ReplicatedStorage:WaitForChild("Remotes")

Players.PlayerAdded:Connect(function(player)
    print(player.Name, remotes, Run.Heartbeat)
end)
