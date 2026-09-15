-- A type and a value: the table carries the value, and the type alias
-- stands beside it.
export type Id = number

const MAX: Id = 10

local function next(id: Id): Id
    return id + 1
end return { MAX = MAX, next = next }
