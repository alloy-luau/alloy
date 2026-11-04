--- A trait with no default body.
local Store = {} export type Store = {
--- Reads a key.
read read: (self: any, key: string) -> string?,
--- Writes a key.
read write: (self: any, key: string, value: string) -> (), -- trailing
} Store.read = (nil :: any) :: (self: any, key: string) -> string? Store.write = (nil :: any) :: (self: any, key: string, value: string) -> ()

--- A trait with a default body between two signatures.
local Named = {} export type Named = {
--- The name, which each impl writes.
read name: (self: any) -> string, read greet: (self: any) -> string, read reset: (self: any) -> () } Named.name = (nil :: any) :: (self: any) -> string
--- The greeting, from the name.
Named["greet@for"] = function(__impl) return function(self) : string
    return "hi " .. (__impl or self).name(self)
  end end Named.greet = Named["greet@for"](nil)
--- A signature after the default body.
Named.reset = (nil :: any) :: (self: any) -> ()


--- An interface with documented fields.
export type Options = {
--- The first field.
first: number,
-- A plain comment on the second field.
second: string?,
} return { Store = Store, Named = Named }
