-- A module of types alone binds no value, and Luau takes exactly one
-- value from a module, so the emit returns an empty table.
export type Id = number

export type Named = { name: string }



export type Pair = { first: Id, second: Id } return {}
