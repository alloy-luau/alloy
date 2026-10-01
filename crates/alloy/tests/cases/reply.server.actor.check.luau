-- A worker Actor that answers: a reply comes back to any script.
local __alloy = require("@alloy") local Light = (__alloy.message("Light", 2) :: any) :: { topic: string, fire: (actor: Actor, job: number, input: buffer) -> (), on: (handler: (job: number, input: buffer, respond: (job: number, levels: buffer) -> ()) -> ()) -> RBXScriptConnection, once: (handler: (job: number, input: buffer, respond: (job: number, levels: buffer) -> ()) -> ()) -> RBXScriptConnection, replied: (handler: (job: number, levels: buffer) -> ()) -> RBXScriptConnection }
local Heat = (__alloy.message("Heat", 1) :: any) :: { topic: string, fire: (actor: Actor, job: number) -> (), on: (handler: (job: number, respond: (job: number) -> ()) -> ()) -> RBXScriptConnection, once: (handler: (job: number, respond: (job: number) -> ()) -> ()) -> RBXScriptConnection, replied: (handler: (job: number) -> ()) -> RBXScriptConnection }

local worker = script.Parent :: Actor

Light.replied(function(job, levels)
    print(job, buffer.len(levels))
end)

Light.fire(worker, 7, buffer.create(8))

Light.on(function(job, input, respond)
    local levels = buffer.create(0)

    task.desynchronize() do
        levels = input
    task.synchronize() end

    respond(job, levels)
end)

Heat.once(function(job, respond)
    respond(job)
    worker.Name = "warm"
end)
