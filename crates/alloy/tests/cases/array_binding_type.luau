-- A list literal takes the element type of its binding in the check
-- artifact, since Luau infers it from the items alone (LANG_BUGS 143).
local __alloy = require("@alloy") const A: number = 1
const B: string = 'b'

const LIST: { unknown } = __alloy.Array.from({ A, B })
const PAIR: __alloy.Array<(number | string)> = __alloy.Array.from({ A, B })
const LOOSE = __alloy.Array.from({ A, B }) return { LIST = LIST, PAIR = PAIR, LOOSE = LOOSE }
