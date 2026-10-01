-- A worker Actor: a message in, a parallel scan, a message out.
local Step = ({ topic = "Step", fire = function(actor: Actor, ...) actor:SendMessage("Step", ...) end } :: any) :: { topic: string, fire: (actor: Actor, dt: number, reply: Actor) -> (), on: (handler: (dt: number, reply: Actor) -> ()) -> RBXScriptConnection, once: (handler: (dt: number, reply: Actor) -> ()) -> RBXScriptConnection }
local Fallen = ({ topic = "Fallen", fire = function(actor: Actor, ...) actor:SendMessage("Fallen", ...) end } :: any) :: { topic: string, fire: (actor: Actor, parts: { BasePart }) -> (), on: (handler: (parts: { BasePart }) -> ()) -> RBXScriptConnection, once: (handler: (parts: { BasePart }) -> ()) -> RBXScriptConnection }

local parts: { BasePart } = {}

Step.on(function(dt, reply)
    local hits = {}

    task.desynchronize() do
        for _, part in parts do
            if part.Position.Y < 0 then
                table.insert(hits, part)
            end
        end
    task.synchronize() end

    Fallen.fire(reply, hits)
end)

Fallen.once(function(found)
    print(#found)
end)
