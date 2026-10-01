-- A worker Actor that answers: a reply comes back to any script.
local __alloy = require("@alloy") local Light = __alloy.message("Light", 2)
local Heat = __alloy.message("Heat", 1)

local worker = script.Parent :: Actor

Light.replied(function(job, levels)
    print(job, buffer.len(levels))
end)

Light.fire(worker, 7, buffer.create(8))

script:GetActor():BindToMessage("Light", __alloy.message_respond(function(job, input, respond)
    local levels = buffer.create(0)

    task.desynchronize() do
        levels = input
    task.synchronize() end

    respond(job, levels)
end, 2, false))

__alloy.message_once(script:GetActor(), "Heat", true, __alloy.message_respond(function(job, respond)
    respond(job)
    worker.Name = "warm"
end, 1, true))
