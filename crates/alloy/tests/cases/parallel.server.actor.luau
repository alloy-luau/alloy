-- A worker Actor: a message in, a parallel scan, a message out.
local __alloy = require("@alloy") local Step = { topic = "Step", fire = function(actor: Actor, ...) actor:SendMessage("Step", ...) end }
local Fallen = { topic = "Fallen", fire = function(actor: Actor, ...) actor:SendMessage("Fallen", ...) end }

local parts: { BasePart } = {}

script:GetActor():BindToMessage("Step", function(dt, reply)
    local hits = {}

    task.desynchronize() do
        for _, part in parts do
            if part.Position.Y < 0 then
                table.insert(hits, part)
            end
        end
    task.synchronize() end

    reply:SendMessage("Fallen", hits)
end)

__alloy.message_once(script:GetActor(), "Fallen", true, function(found)
    print(#found)
end)
