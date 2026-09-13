-- An attribute in a name list carries the `@` it is applied with; the
-- local name after `as` is written plain. `import * as M, { a }` takes
-- the whole module and names from it, the way a default binding does.
local _m1 = require("./theme") local tagged = _m1.tagged
local _m2 = require("./theme") local tag, swatch = _m2.tagged, _m2.swatch
local All = require("./theme") local other = All.swatch

print(tagged, tag, swatch, All, other)
