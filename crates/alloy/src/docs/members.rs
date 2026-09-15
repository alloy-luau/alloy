//! The members of the std types: `HashMap:get`, `Array:push`, and
//! the rest, one section per member.

use super::lookup;

/// What one member of a std type is: a function on the type table, a
/// function on a value, or a plain member.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemberKind {
    Static,
    Method,
    Field,
    Constant,
}

impl MemberKind {
    /// The word `alloy doc --json` writes.
    pub fn name(self) -> &'static str {
        match self {
            MemberKind::Static => "static",
            MemberKind::Method => "method",
            MemberKind::Field => "field",
            MemberKind::Constant => "constant",
        }
    }

    /// The heading a run of members of this kind sits under.
    pub fn heading(self) -> &'static str {
        match self {
            MemberKind::Static => "Statics",
            MemberKind::Method => "Methods",
            MemberKind::Field => "Fields",
            MemberKind::Constant => "Constants",
        }
    }
}

/// The order the headings print in.
pub const MEMBER_KINDS: [MemberKind; 4] = [
    MemberKind::Static,
    MemberKind::Method,
    MemberKind::Field,
    MemberKind::Constant,
];

/// One documented member of a std type. A reference page gives each one
/// a section: the signature, what it does, and an example.
pub struct Member {
    pub name: &'static str,
    pub kind: MemberKind,
    /// The signature as Alloy prints it: `HashMap:get(key: K): V?`.
    pub signature: &'static str,
    pub doc: &'static str,
    /// Alloy that `alloy check` accepts on its own. The test
    /// `every_member_example_compiles` holds that.
    pub example: &'static str,
}

/// The members of every std type, by the key of its entry in `TABLE`.
pub const MEMBERS: &[(&str, &[Member])] = &[
    (
        "HashMap",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "HashMap.new<K, V>(): HashMap<K, V>",
                doc: "An empty map. The annotation on the binding fixes `K` and `V`, since an empty map infers neither.",
                example: "local prices: HashMap<string, number> = HashMap.new()\nprices:set(\"sword\", 10)",
            },
            Member {
                name: "from",
                kind: MemberKind::Static,
                signature: "HashMap.from<K, V>(t: { [K]: V }): HashMap<K, V>",
                doc: "A new map with the pairs of a plain table. The table itself is left alone.",
                example: "local prices = HashMap.from({ sword = 10, pet = 25 })\nprint(prices:len())",
            },
            Member {
                name: "get",
                kind: MemberKind::Method,
                signature: "HashMap:get(key: K): V?",
                doc: "The value under `key`, or nil when the map has no such key. The result is optional, so guard it or end the chain with `??`.",
                example: "local prices = HashMap.from({ sword = 10 })\nlocal price = prices:get(\"sword\") ?? 0\nprint(price)",
            },
            Member {
                name: "set",
                kind: MemberKind::Method,
                signature: "HashMap:set(key: K, value: V): HashMap<K, V>",
                doc: "Stores `value` under `key` and returns the map, so calls chain. A `set` to nil removes the key and drops the count, so `len` stays true.",
                example: "local prices: HashMap<string, number> = HashMap.new()\nprices:set(\"gem\", 5):set(\"sword\", 10)\nprint(prices:len())",
            },
            Member {
                name: "has",
                kind: MemberKind::Method,
                signature: "HashMap:has(key: K): boolean",
                doc: "Whether the map holds `key`. A key whose value is nil is not held.",
                example: "local prices = HashMap.from({ gem = 5 })\nif prices:has(\"gem\") then\n    print(\"in stock\")\nend",
            },
            Member {
                name: "remove",
                kind: MemberKind::Method,
                signature: "HashMap:remove(key: K): V?",
                doc: "Removes `key` and returns the value it held, or nil when there was none.",
                example: "local prices = HashMap.from({ gem = 5 })\nlocal old = prices:remove(\"gem\")\nprint(old)",
            },
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "HashMap:len(): number",
                doc: "The number of keys. The map counts as keys come and go, so this costs the same at any size.",
                example: "local prices = HashMap.from({ gem = 5, sword = 10 })\nprint(prices:len())",
            },
            Member {
                name: "keys",
                kind: MemberKind::Method,
                signature: "HashMap:keys(): K[]",
                doc: "The keys as an array, in no set order. The array carries the Array methods.",
                example: "local prices = HashMap.from({ gem = 5 })\nprint(prices:keys():join(\", \"))",
            },
            Member {
                name: "values",
                kind: MemberKind::Method,
                signature: "HashMap:values(): V[]",
                doc: "The values as an array, in the order `keys` gives.",
                example: "local prices = HashMap.from({ gem = 5 })\nprint(prices:values():len())",
            },
            Member {
                name: "entries",
                kind: MemberKind::Method,
                signature: "HashMap:entries(): () -> (K?, V?)",
                doc: "An iterator over the pairs, for a `for` loop. The order is the table's own.",
                example: "local prices = HashMap.from({ gem = 5 })\nfor key, value in prices:entries() do\n    print(key, value)\nend",
            },
            Member {
                name: "get_or_insert",
                kind: MemberKind::Method,
                signature: "HashMap:get_or_insert(key: K, default: V): V",
                doc: "The value under `key`. When there is none the map stores `default` first, so the result is never nil.",
                example: "local counts: HashMap<string, number> = HashMap.new()\nlocal n = counts:get_or_insert(\"hits\", 0)\nprint(n)",
            },
            Member {
                name: "clear",
                kind: MemberKind::Method,
                signature: "HashMap:clear()",
                doc: "Removes every key and sets the count to zero. The map itself stays, so every reference to it sees the empty map.",
                example: "local prices = HashMap.from({ gem = 5 })\nprices:clear()",
            },
        ],
    ),
    (
        "Set",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "Set.new<T>(): Set<T>",
                doc: "An empty set. The annotation on the binding fixes `T`.",
                example: "local seen: Set<string> = Set.new()\nseen:add(\"a\")",
            },
            Member {
                name: "from",
                kind: MemberKind::Static,
                signature: "Set.from<T>(items: { T }): Set<T>",
                doc: "A new set with the items of an array. A repeated item joins once.",
                example: "local seen = Set.from({ 1, 2, 2, 3 })\nprint(seen:len())",
            },
            Member {
                name: "add",
                kind: MemberKind::Method,
                signature: "Set:add(value: T): Set<T>",
                doc: "Adds `value` and returns the set, so calls chain. Adding a member again changes nothing.",
                example: "local seen: Set<string> = Set.new()\nif seen:add(\"a\"):has(\"a\") then\n    print(\"added\")\nend",
            },
            Member {
                name: "has",
                kind: MemberKind::Method,
                signature: "Set:has(value: T): boolean",
                doc: "Whether `value` is a member.",
                example: "local seen = $set[1, 2, 3]\nprint(seen:has(2))",
            },
            Member {
                name: "remove",
                kind: MemberKind::Method,
                signature: "Set:remove(value: T): boolean",
                doc: "Removes `value` and says whether it was there.",
                example: "local seen = $set[1, 2]\nprint(seen:remove(1))",
            },
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "Set:len(): number",
                doc: "The number of members. The set counts as members come and go.",
                example: "local seen = $set[1, 2, 3]\nprint(seen:len())",
            },
            Member {
                name: "union",
                kind: MemberKind::Method,
                signature: "Set:union(other: Set<T>): Set<T>",
                doc: "A new set of the members of both. Neither input changes.",
                example: "local a = $set[1, 2]\nlocal b = $set[2, 3]\nprint(a:union(b):len())",
            },
            Member {
                name: "intersection",
                kind: MemberKind::Method,
                signature: "Set:intersection(other: Set<T>): Set<T>",
                doc: "A new set of the members that both hold.",
                example: "local a = $set[1, 2]\nlocal b = $set[2, 3]\nprint(a:intersection(b):len())",
            },
            Member {
                name: "difference",
                kind: MemberKind::Method,
                signature: "Set:difference(other: Set<T>): Set<T>",
                doc: "A new set of this set's members that `other` does not hold.",
                example: "local a = $set[1, 2]\nlocal b = $set[2, 3]\nprint(a:difference(b):len())",
            },
            Member {
                name: "to_array",
                kind: MemberKind::Method,
                signature: "Set:to_array(): T[]",
                doc: "The members as an array, in no set order.",
                example: "local seen = $set[1, 2, 3]\nprint(seen:to_array():len())",
            },
        ],
    ),
    (
        "Array",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "Array.new<T>(): T[]",
                doc: "An empty array with the Array metatable. `[ ]` is the literal for one.",
                example: "local xs: number[] = Array.new()\nxs:push(1)",
            },
            Member {
                name: "with_capacity",
                kind: MemberKind::Static,
                signature: "Array.with_capacity<T>(n: number): T[]",
                doc: "An empty array with room for `n` items already allocated. Use it before a loop that pushes a known count.",
                example: "local xs: number[] = Array.with_capacity(3)\nxs:push(1, 2, 3)",
            },
            Member {
                name: "from",
                kind: MemberKind::Static,
                signature: "Array.from<T>(t: { T }): T[]",
                doc: "The table `t` as an array: it gains the metatable and stays the same table, so no copy happens.",
                example: "local raw = { 1, 2, 3 }\nlocal xs = Array.from(raw)\nprint(xs:len())",
            },
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "Array:len(): number",
                doc: "The number of items, the `#` of the table.",
                example: "local xs = [ 1, 2, 3 ]\nprint(xs:len())",
            },
            Member {
                name: "is_empty",
                kind: MemberKind::Method,
                signature: "Array:is_empty(): boolean",
                doc: "Whether the array holds no items.",
                example: "local xs: number[] = [ ]\nprint(xs:is_empty())",
            },
            Member {
                name: "push",
                kind: MemberKind::Method,
                signature: "Array:push(...: T): Array<T>",
                doc: "Appends the values in the order given. It writes into the array and returns it, so calls chain.",
                example: "local xs = [ 1 ]\nxs:push(2, 3):push(4)\nprint(xs:len())",
            },
            Member {
                name: "pop",
                kind: MemberKind::Method,
                signature: "Array:pop(): T?",
                doc: "Removes the last item and returns it, or nil when the array is empty.",
                example: "local xs = [ 1, 2 ]\nprint(xs:pop())",
            },
            Member {
                name: "first",
                kind: MemberKind::Method,
                signature: "Array:first(): T?",
                doc: "The first item, or nil when the array is empty.",
                example: "local xs = [ 1, 2 ]\nprint(xs:first())",
            },
            Member {
                name: "last",
                kind: MemberKind::Method,
                signature: "Array:last(): T?",
                doc: "The last item, or nil when the array is empty.",
                example: "local xs = [ 1, 2 ]\nprint(xs:last())",
            },
            Member {
                name: "map",
                kind: MemberKind::Method,
                signature: "Array:map<U>(f: (T, number) -> U): U[]",
                doc: "A new array of `f(item, index)` for each item. The result names a second alias of the same shape, so the third `map` in one chain types as `any`.",
                example: "local xs = [ 1, 2, 3 ]\nlocal doubled = xs:map(function(x) return x * 2 end)\nprint(doubled:join(\", \"))",
            },
            Member {
                name: "filter",
                kind: MemberKind::Method,
                signature: "Array:filter(f: (T, number) -> boolean): T[]",
                doc: "A new array of the items where `f(item, index)` holds.",
                example: "local xs = [ 1, 2, 3, 4 ]\nlocal even = xs:filter(function(x) return x % 2 == 0 end)\nprint(even:len())",
            },
            Member {
                name: "find",
                kind: MemberKind::Method,
                signature: "Array:find(f: (T, number) -> boolean): T?",
                doc: "The first item where `f(item, index)` holds, or nil.",
                example: "local xs = [ 1, 2, 3 ]\nprint(xs:find(function(x) return x > 1 end))",
            },
            Member {
                name: "find_index",
                kind: MemberKind::Method,
                signature: "Array:find_index(f: (T, number) -> boolean): number?",
                doc: "The index of the first item where `f(item, index)` holds, or nil.",
                example: "local xs = [ 1, 2, 3 ]\nprint(xs:find_index(function(x) return x > 1 end))",
            },
            Member {
                name: "contains",
                kind: MemberKind::Method,
                signature: "Array:contains(value: T): boolean",
                doc: "Whether an item equals `value`. `value in xs` is the same test.",
                example: "local xs = [ 1, 2, 3 ]\nprint(xs:contains(2))",
            },
            Member {
                name: "index_of",
                kind: MemberKind::Method,
                signature: "Array:index_of(value: T): number?",
                doc: "The index of the first item that equals `value`, or nil.",
                example: "local xs = [ \"a\", \"b\" ]\nprint(xs:index_of(\"b\"))",
            },
            Member {
                name: "for_each",
                kind: MemberKind::Method,
                signature: "Array:for_each(f: (T, number) -> ())",
                doc: "Calls `f(item, index)` for each item, in order, and returns nothing.",
                example: "local xs = [ 1, 2 ]\nxs:for_each(function(x, i) print(i, x) end)",
            },
            Member {
                name: "reduce",
                kind: MemberKind::Method,
                signature: "Array:reduce<U>(f: (U, T, number) -> U, init: U): U",
                doc: "Folds the array: `acc = f(acc, item, index)` from `init`, and the last `acc` is the result. Annotate the accumulator when its body needs the type.",
                example: "local xs = [ 1, 2, 3 ]\nlocal sum = xs:reduce(function(acc: number, x) return acc + x end, 0)\nprint(sum)",
            },
            Member {
                name: "slice",
                kind: MemberKind::Method,
                signature: "Array:slice(from: number, to: number?): T[]",
                doc: "A new array of the items from index `from` to index `to`, both inclusive. `to` defaults to the last index.",
                example: "local xs = [ 1, 2, 3, 4 ]\nprint(xs:slice(2, 3):join(\", \"))",
            },
            Member {
                name: "concat",
                kind: MemberKind::Method,
                signature: "Array:concat(other: T[]): T[]",
                doc: "A new array of this one's items followed by `other`'s. Neither input changes.",
                example: "local xs = [ 1, 2 ]\nprint(xs:concat([ 3 ]):len())",
            },
            Member {
                name: "reverse",
                kind: MemberKind::Method,
                signature: "Array:reverse(): T[]",
                doc: "A new array with the items in the other order. This one is left as it was.",
                example: "local xs = [ 1, 2, 3 ]\nprint(xs:reverse():first())",
            },
            Member {
                name: "sort_by",
                kind: MemberKind::Method,
                signature: "Array:sort_by(less: (T, T) -> boolean): T[]",
                doc: "Sorts in place with `less(a, b)` and returns the same array, so a chain reads on. The sort is not stable.",
                example: "local xs = [ 3, 1, 2 ]\nxs:sort_by(function(a, b) return a < b end)\nprint(xs:join(\", \"))",
            },
            Member {
                name: "join",
                kind: MemberKind::Method,
                signature: "Array:join(sep: string?): string",
                doc: "Every item through `tostring`, joined by `sep`. Without `sep` the parts run together.",
                example: "local xs = [ 1, 2, 3 ]\nprint(xs:join(\", \"))",
            },
        ],
    ),
    (
        "ReadArray",
        &[
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "ReadArray:len(): number",
                doc: "The number of items.",
                example: "local xs: read number[] = [ 1, 2, 3 ]\nprint(xs:len())",
            },
            Member {
                name: "is_empty",
                kind: MemberKind::Method,
                signature: "ReadArray:is_empty(): boolean",
                doc: "Whether the array holds no items.",
                example: "local xs: read number[] = [ ]\nprint(xs:is_empty())",
            },
            Member {
                name: "first",
                kind: MemberKind::Method,
                signature: "ReadArray:first(): T?",
                doc: "The first item, or nil when the array is empty.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:first())",
            },
            Member {
                name: "last",
                kind: MemberKind::Method,
                signature: "ReadArray:last(): T?",
                doc: "The last item, or nil when the array is empty.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:last())",
            },
            Member {
                name: "map",
                kind: MemberKind::Method,
                signature: "ReadArray:map<U>(f: (T, number) -> U): U[]",
                doc: "A new array of `f(item, index)` for each item. The result is a writable `Array<U>`.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:map(function(x) return x * 2 end):len())",
            },
            Member {
                name: "filter",
                kind: MemberKind::Method,
                signature: "ReadArray:filter(f: (T, number) -> boolean): T[]",
                doc: "A new array of the items where `f(item, index)` holds.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:filter(function(x) return x > 1 end):len())",
            },
            Member {
                name: "find",
                kind: MemberKind::Method,
                signature: "ReadArray:find(f: (T, number) -> boolean): T?",
                doc: "The first item where `f(item, index)` holds, or nil.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:find(function(x) return x > 1 end))",
            },
            Member {
                name: "find_index",
                kind: MemberKind::Method,
                signature: "ReadArray:find_index(f: (T, number) -> boolean): number?",
                doc: "The index of the first item where `f(item, index)` holds, or nil.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:find_index(function(x) return x > 1 end))",
            },
            Member {
                name: "contains",
                kind: MemberKind::Method,
                signature: "ReadArray:contains(value: T): boolean",
                doc: "Whether an item equals `value`.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:contains(2))",
            },
            Member {
                name: "index_of",
                kind: MemberKind::Method,
                signature: "ReadArray:index_of(value: T): number?",
                doc: "The index of the first item that equals `value`, or nil.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:index_of(2))",
            },
            Member {
                name: "for_each",
                kind: MemberKind::Method,
                signature: "ReadArray:for_each(f: (T, number) -> ())",
                doc: "Calls `f(item, index)` for each item, in order.",
                example: "local xs: read number[] = [ 1, 2 ]\nxs:for_each(function(x) print(x) end)",
            },
            Member {
                name: "reduce",
                kind: MemberKind::Method,
                signature: "ReadArray:reduce<U>(f: (U, T, number) -> U, init: U): U",
                doc: "Folds the array from `init`. This is how a function that takes a `read T[]` sums it.",
                example: "local function total(xs: read number[]): number\n    return xs:reduce(function(acc: number, x) return acc + x end, 0)\nend\nprint(total([ 1, 2 ]))",
            },
            Member {
                name: "slice",
                kind: MemberKind::Method,
                signature: "ReadArray:slice(from: number, to: number?): T[]",
                doc: "A new array of the items from `from` to `to`, both inclusive. The result is writable.",
                example: "local xs: read number[] = [ 1, 2, 3 ]\nprint(xs:slice(2):len())",
            },
            Member {
                name: "concat",
                kind: MemberKind::Method,
                signature: "ReadArray:concat(other: T[]): T[]",
                doc: "A new array of this one's items followed by `other`'s.",
                example: "local xs: read number[] = [ 1 ]\nprint(xs:concat([ 2 ]):len())",
            },
            Member {
                name: "join",
                kind: MemberKind::Method,
                signature: "ReadArray:join(sep: string?): string",
                doc: "Every item through `tostring`, joined by `sep`.",
                example: "local xs: read number[] = [ 1, 2 ]\nprint(xs:join(\", \"))",
            },
        ],
    ),
    (
        "WriteArray",
        &[Member {
            name: "push",
            kind: MemberKind::Method,
            signature: "WriteArray:push(...: T): WriteArray<T>",
            doc: "Appends the values in the order given and returns the sink, so calls chain. It is the one method a `write T[]` has: a sink takes items and never reads them back.",
            example: "local function log_to(sink: write string[])\n    sink:push(\"line\")\nend\nlog_to([ ])",
        }],
    ),
    (
        "Future",
        &[
            Member {
                name: "resolve",
                kind: MemberKind::Static,
                signature: "Future.resolve<T>(value: T): Future<T>",
                doc: "A Future that has already settled with `value`. Every `await` on it returns at once.",
                example: "local ready = Future.resolve(42)\nasync do print(await ready) end",
            },
            Member {
                name: "reject",
                kind: MemberKind::Static,
                signature: "Future.reject(error: any): Future<any>",
                doc: "A Future that has already failed with `error`. An `await` on it rethrows, and `try await` returns that `Err` from the function. The payload reads `any`: a rejected Future never settles with one, so it stands where a `Future<T>` is asked for.",
                example: "local bad = Future.reject(\"no disk\")\nbad:andThen(nil, function(e) print(e) end)",
            },
            Member {
                name: "delay",
                kind: MemberKind::Static,
                signature: "Future.delay(seconds: number): Future<()>",
                doc: "A Future that settles after the wait, with no value. It is the async form of `task.wait`.",
                example: "async do\n    await Future.delay(0.5)\n    print(\"later\")\nend",
            },
            Member {
                name: "all",
                kind: MemberKind::Static,
                signature: "Future.all<T>(futures: Future<T>[]): Future<T[]>",
                doc: "One Future of every value, in the order of the list. The first failure fails the whole thing, and the rest keep running. Every Future in the list carries one type here; `race` and `any` are the ones that take a mixed list.",
                example: "async do\n    local both = await Future.all([Future.resolve(1), Future.resolve(2)])\n    print(both:len())\nend",
            },
            Member {
                name: "race",
                kind: MemberKind::Static,
                signature: "Future.race<T>(futures: Future<T>[]): Future<T>",
                doc: "The first Future to settle, whether it succeeds or fails. Use it for a timeout beside real work. The futures in the list need not carry one type: the result is the union of their value types, so a value beside a `Future.delay` is optional.",
                example: "async do\n    local first = await Future.race([Future.resolve(1), Future.resolve(2)])\n    print(first)\nend",
            },
            Member {
                name: "any",
                kind: MemberKind::Static,
                signature: "Future.any<T>(futures: Future<T>[]): Future<T>",
                doc: "The first Future to succeed. It fails only when every one of them fails. As with `race`, a list whose futures carry different types gives the union of them.",
                example: "async do\n    local ok = await Future.any([Future.reject(\"a\"), Future.resolve(2)])\n    print(ok)\nend",
            },
            Member {
                name: "all_settled",
                kind: MemberKind::Static,
                signature: "Future.all_settled<T>(futures: Future<T>[]): Future<Result<T, any>[]>",
                doc: "One Result per Future, in order. No failure fails it. The error side is `any`, since a rejection carries whatever it was rejected with.",
                example: "async do\n    local rs = await Future.all_settled([Future.resolve(1), Future.reject(\"no\")])\n    print(rs:len())\nend",
            },
            Member {
                name: "andThen",
                kind: MemberKind::Method,
                signature: "Future:andThen(on_resolve: ((T) -> ...any)?, on_reject: ((any) -> ...any)?): Future<T>",
                doc: "Runs a callback when the Future settles and returns the same Future, so calls chain. What the callback returns is dropped: the Future keeps its own value.",
                example: "local f = Future.resolve(1):andThen(function(n) print(n) end)\nasync do print(await f) end",
            },
            Member {
                name: "and_then",
                kind: MemberKind::Method,
                signature: "Future:and_then(on_resolve: ((T) -> ...any)?, on_reject: ((any) -> ...any)?): Future<T>",
                doc: "The same function under a snake_case name. `andThen` is the one camelCase name the std carries, so a file that keeps to one spelling writes this.",
                example: "local f = Future.resolve(1):and_then(function(n) print(n) end)\nasync do print(await f) end",
            },
            Member {
                name: "cancel",
                kind: MemberKind::Method,
                signature: "Future:cancel()",
                doc: "Closes the task behind the Future. An `await` on a cancelled Future raises.",
                example: "local slow = Future.delay(10)\nslow:cancel()",
            },
            Member {
                name: "is_settled",
                kind: MemberKind::Method,
                signature: "Future:is_settled(): boolean",
                doc: "Whether the Future has a value or a failure. A cancelled Future is not settled.",
                example: "local ready = Future.resolve(1)\nprint(ready:is_settled())",
            },
        ],
    ),
    (
        "Result",
        &[
            Member {
                name: "pcall",
                kind: MemberKind::Static,
                signature: "Result.pcall<T>(f: (...any) -> T, ...: any): Result<T, string>",
                doc: "Calls `f` with the arguments after it: `Ok` of what it returns, or `Err` of what it threw. The value type follows `f`, and the `Err` carries the traceback in `trace`.",
                example: "local r = Result.pcall(string.rep, \"a\", 3)\nprint(r:unwrap_or(\"\"))",
            },
            Member {
                name: "unwrap",
                kind: MemberKind::Method,
                signature: "Result:unwrap(): T",
                doc: "The value of an `Ok`. An `Err` raises, with the error and its trace in the message. Reach for it when an `Err` is a bug, not a case.",
                example: "local r: Result<number, string> = Ok(1)\nprint(r:unwrap())",
            },
            Member {
                name: "expect",
                kind: MemberKind::Method,
                signature: "Result:expect(message: string): T",
                doc: "The value of an `Ok`. An `Err` raises with `message`, which says what the caller expected.",
                example: "local r: Result<number, string> = Ok(1)\nprint(r:expect(\"the config sets a width\"))",
            },
            Member {
                name: "unwrap_or",
                kind: MemberKind::Method,
                signature: "Result:unwrap_or<D>(default: D): T | D",
                doc: "The value of an `Ok`, or `default`. The result type is `T | D`, so `r:unwrap_or(nil)` is `T?`.",
                example: "local r: Result<number, string> = Err(\"no\")\nprint(r:unwrap_or(0))",
            },
            Member {
                name: "map",
                kind: MemberKind::Method,
                signature: "Result:map<U>(f: (T) -> U): Result<U, E>",
                doc: "`Ok(f(value))`, or the same `Err` untouched. The mapped Result names a second alias, so a third `map` in one chain types as `any`.",
                example: "local r: Result<number, string> = Ok(2)\nprint(r:map(function(n) return n * 2 end):unwrap())",
            },
            Member {
                name: "map_err",
                kind: MemberKind::Method,
                signature: "Result:map_err<F>(f: (E) -> F): Result<T, F>",
                doc: "`Err(f(error))`, or the same `Ok` untouched. Use it to turn a low level error into one the caller names.",
                example: "local r: Result<number, string> = Err(\"io\")\nprint(r:map_err(function(e) return `read: {e}` end):is_err())",
            },
            Member {
                name: "is_ok",
                kind: MemberKind::Method,
                signature: "Result:is_ok(): boolean",
                doc: "Whether the Result is an `Ok`. It narrows nothing; a `match` or `if local Ok(v) = r` binds the value.",
                example: "local r: Result<number, string> = Ok(1)\nprint(r:is_ok())",
            },
            Member {
                name: "is_err",
                kind: MemberKind::Method,
                signature: "Result:is_err(): boolean",
                doc: "Whether the Result is an `Err`.",
                example: "local r: Result<number, string> = Err(\"no\")\nprint(r:is_err())",
            },
            Member {
                name: "ok",
                kind: MemberKind::Method,
                signature: "Result:ok(): T?",
                doc: "The value of an `Ok`, or nil for an `Err`. It drops the error, which is what `??` wants on the other side.",
                example: "local r: Result<number, string> = Ok(1)\nprint(r:ok() ?? 0)",
            },
            Member {
                name: "tag",
                kind: MemberKind::Field,
                signature: "Result.tag: \"Ok\" | \"Err\"",
                doc: "Which case the Result is, as a string. A `match` reads it for you; the field is there for code that stores or sends the tag.",
                example: "local r: Result<number, string> = Ok(1)\nprint(r.tag)",
            },
            Member {
                name: "trace",
                kind: MemberKind::Field,
                signature: "Result.trace: string?",
                doc: "The traceback of an `Err`. `try` and `Result.pcall` fill it in, `Err(e, trace)` sets it by hand, and `unwrap` prints it under the error.",
                example: "local r = Result.pcall(error, \"boom\")\nprint(r.trace ?? \"no trace\")",
            },
        ],
    ),
    (
        "Queue",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "Queue.new<T>(): Queue<T>",
                doc: "An empty queue. The annotation on the binding fixes `T`.",
                example: "local jobs: Queue<string> = Queue.new()\njobs:push(\"build\")",
            },
            Member {
                name: "from",
                kind: MemberKind::Static,
                signature: "Queue.from<T>(items: { T }): Queue<T>",
                doc: "A queue filled from an array, front to back in the array's order.",
                example: "local jobs = Queue.from({ \"build\", \"test\" })\nprint(jobs:pop())",
            },
            Member {
                name: "push",
                kind: MemberKind::Method,
                signature: "Queue:push(value: T)",
                doc: "Appends `value` at the back. The ring of indices makes this cost the same at any size.",
                example: "local jobs: Queue<string> = Queue.new()\njobs:push(\"build\")",
            },
            Member {
                name: "pop",
                kind: MemberKind::Method,
                signature: "Queue:pop(): T?",
                doc: "Removes the front item and returns it, or nil when the queue is empty. An emptied queue starts its indices over, so they never run away.",
                example: "local jobs = Queue.from({ \"build\" })\nwhile local job = jobs:pop() do\n    print(job)\nend",
            },
            Member {
                name: "peek",
                kind: MemberKind::Method,
                signature: "Queue:peek(): T?",
                doc: "The front item without removing it, or nil when the queue is empty.",
                example: "local jobs = Queue.from({ \"build\" })\nprint(jobs:peek())",
            },
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "Queue:len(): number",
                doc: "The number of items waiting.",
                example: "local jobs = Queue.from({ \"build\", \"test\" })\nprint(jobs:len())",
            },
            Member {
                name: "is_empty",
                kind: MemberKind::Method,
                signature: "Queue:is_empty(): boolean",
                doc: "Whether the queue holds nothing.",
                example: "local jobs: Queue<string> = Queue.new()\nprint(jobs:is_empty())",
            },
            Member {
                name: "clear",
                kind: MemberKind::Method,
                signature: "Queue:clear()",
                doc: "Drops every item and resets the indices.",
                example: "local jobs = Queue.from({ \"build\" })\njobs:clear()",
            },
            Member {
                name: "to_array",
                kind: MemberKind::Method,
                signature: "Queue:to_array(): T[]",
                doc: "The items front to back, as a new array. The queue keeps them; a `for` loop over the queue reads the same order without a pop.",
                example: "local jobs = Queue.from({ \"build\", \"test\" })\nprint(jobs:to_array():join(\", \"))",
            },
        ],
    ),
    (
        "Heap",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "Heap.new<T>(less: ((T, T) -> boolean)?): Heap<T>",
                doc: "An empty heap. `less` decides which value is least and defaults to `<`, so numbers and strings need none and tables take one.",
                example: "local open = Heap.new(function(a: number, b: number) return a < b end)\nopen:push(3)",
            },
            Member {
                name: "from",
                kind: MemberKind::Static,
                signature: "Heap.from<T>(items: { T }, less: ((T, T) -> boolean)?): Heap<T>",
                doc: "A heap built from an array, pushed item by item.",
                example: "local costs = Heap.from({ 3, 1, 2 })\nprint(costs:pop())",
            },
            Member {
                name: "push",
                kind: MemberKind::Method,
                signature: "Heap:push(value: T)",
                doc: "Adds a value and sifts it up. The cost grows with the log of the size.",
                example: "local costs: Heap<number> = Heap.new()\ncosts:push(3)",
            },
            Member {
                name: "pop",
                kind: MemberKind::Method,
                signature: "Heap:pop(): T?",
                doc: "Removes the least value under `less` and returns it, or nil when the heap is empty.",
                example: "local costs = Heap.from({ 3, 1, 2 })\nprint(costs:pop())",
            },
            Member {
                name: "peek",
                kind: MemberKind::Method,
                signature: "Heap:peek(): T?",
                doc: "The least value without removing it, or nil when the heap is empty.",
                example: "local costs = Heap.from({ 3, 1 })\nprint(costs:peek())",
            },
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "Heap:len(): number",
                doc: "The number of values held.",
                example: "local costs = Heap.from({ 3, 1 })\nprint(costs:len())",
            },
            Member {
                name: "is_empty",
                kind: MemberKind::Method,
                signature: "Heap:is_empty(): boolean",
                doc: "Whether the heap holds nothing.",
                example: "local costs: Heap<number> = Heap.new()\nprint(costs:is_empty())",
            },
            Member {
                name: "clear",
                kind: MemberKind::Method,
                signature: "Heap:clear()",
                doc: "Drops every value.",
                example: "local costs = Heap.from({ 3, 1 })\ncosts:clear()",
            },
            Member {
                name: "to_array",
                kind: MemberKind::Method,
                signature: "Heap:to_array(): T[]",
                doc: "The values sorted under `less`, as a new array. The heap keeps its own; a `for` loop over the heap reads the same order without a pop.",
                example: "local costs = Heap.from({ 3, 1, 2 })\nprint(costs:to_array():first())",
            },
        ],
    ),
    (
        "Scope",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "Scope.new(): Scope",
                doc: "An empty cleanup bag.",
                example: "local scope = Scope.new()\nprint(scope:len())",
            },
            Member {
                name: "add",
                kind: MemberKind::Method,
                signature: "Scope:add<T>(item: T, owner: Instance?): T",
                doc: "Adds a cleanup and gives `item` straight back, so a binding reads as it did before. It takes anything `delete` accepts, or a function. `owner` names the Instance the item belongs to: `delete` on that Instance cleans the item before the Destroy.",
                example: "local scope = Scope.new()\nlocal stop = scope:add(function() print(\"bye\") end)\nprint(stop ~= nil)",
            },
            Member {
                name: "remove",
                kind: MemberKind::Method,
                signature: "Scope:remove(item: any): boolean",
                doc: "Takes a cleanup out without running it, and says whether it was there.",
                example: "local scope = Scope.new()\nlocal f = scope:add(function() end)\nprint(scope:remove(f))",
            },
            Member {
                name: "clean",
                kind: MemberKind::Method,
                signature: "Scope:clean()",
                doc: "Runs every cleanup, newest first, and empties the bag. A function is called; anything else goes through `delete`.",
                example: "local scope = Scope.new()\nscope:add(function() print(\"bye\") end)\nscope:clean()",
            },
            Member {
                name: "extend",
                kind: MemberKind::Method,
                signature: "Scope:extend(): Scope",
                doc: "A child scope, already added to this one, so cleaning the parent cleans the child.",
                example: "local scope = Scope.new()\nlocal child = scope:extend()\nchild:add(function() end)",
            },
            Member {
                name: "len",
                kind: MemberKind::Method,
                signature: "Scope:len(): number",
                doc: "The number of cleanups the bag holds.",
                example: "local scope = Scope.new()\nscope:add(function() end)\nprint(scope:len())",
            },
            Member {
                name: "Destroy",
                kind: MemberKind::Method,
                signature: "Scope:Destroy()",
                doc: "The same function as `clean`, under the name `delete` calls. `delete scope` is the form to write.",
                example: "local scope = Scope.new()\nscope:add(function() end)\ndelete scope",
            },
        ],
    ),
    (
        "Iter",
        &[
            Member {
                name: "from",
                kind: MemberKind::Static,
                signature: "Iter.from<T>(source: T[] | (() -> T?) | Set<T> | Queue<T> | Heap<T> | HashMap<any, T>): Iter<T>",
                doc: "A lazy iterator over an array, a function that returns the next value or nil, or a std collection. It keeps the element type, so `Iter.from(number[])` collects back to `number[]`.",
                example: "local xs = Iter.from([ 1, 2, 3 ]):collect()\nprint(xs:len())",
            },
            Member {
                name: "range",
                kind: MemberKind::Static,
                signature: "Iter.range(from: number, to: number, step: number?): Iter<number>",
                doc: "The numbers from `from` to `to`, both inclusive, by `step`. `step` defaults to 1.",
                example: "for i in Iter.range(1, 10, 2) do\n    print(i)\nend",
            },
            Member {
                name: "next",
                kind: MemberKind::Method,
                signature: "Iter:next(): T?",
                doc: "Pulls one value, or nil at the end. Every other reader is built on it.",
                example: "local it = Iter.from([ 1, 2 ])\nprint(it:next())",
            },
            Member {
                name: "map",
                kind: MemberKind::Method,
                signature: "Iter:map<U>(f: (T) -> U): Iter<U>",
                doc: "Each value as `f(value)`, still lazily. As with `Array`, the third `map` in one chain types as `any`.",
                example: "local names = Iter.from([ 1, 2 ]):map(function(n) return `#{n}` end)\nprint(names:collect():join(\", \"))",
            },
            Member {
                name: "filter",
                kind: MemberKind::Method,
                signature: "Iter:filter(f: (T) -> boolean): Iter<T>",
                doc: "The values where `f(value)` holds. Nothing runs until something pulls.",
                example: "local big = Iter.from([ 1, 5 ]):filter(function(n) return n > 2 end)\nprint(big:count())",
            },
            Member {
                name: "take",
                kind: MemberKind::Method,
                signature: "Iter:take(n: number): Iter<T>",
                doc: "The first `n` values, then the end. The source is never pulled past them.",
                example: "print(Iter.range(1, 100):take(3):count())",
            },
            Member {
                name: "skip",
                kind: MemberKind::Method,
                signature: "Iter:skip(n: number): Iter<T>",
                doc: "Everything after the first `n` values.",
                example: "print(Iter.range(1, 5):skip(3):count())",
            },
            Member {
                name: "take_while",
                kind: MemberKind::Method,
                signature: "Iter:take_while(f: (T) -> boolean): Iter<T>",
                doc: "Values until `f(value)` first fails; that value and the rest are dropped.",
                example: "local it = Iter.range(1, 10):take_while(function(n) return n < 4 end)\nprint(it:count())",
            },
            Member {
                name: "chain",
                kind: MemberKind::Method,
                signature: "Iter:chain(other: Iter<T>): Iter<T>",
                doc: "This iterator, then `other`, as one run.",
                example: "local both = Iter.from([ 1 ]):chain(Iter.from([ 2 ]))\nprint(both:count())",
            },
            Member {
                name: "collect",
                kind: MemberKind::Method,
                signature: "Iter:collect(): T[]",
                doc: "Pulls every value into an array. This is what ends a chain.",
                example: "local xs = Iter.range(1, 3):collect()\nprint(xs:join(\", \"))",
            },
            Member {
                name: "for_each",
                kind: MemberKind::Method,
                signature: "Iter:for_each(f: (T) -> ())",
                doc: "Pulls every value and calls `f` on it.",
                example: "Iter.from([ 1, 2 ]):for_each(function(n) print(n) end)",
            },
            Member {
                name: "count",
                kind: MemberKind::Method,
                signature: "Iter:count(): number",
                doc: "Pulls every value and returns how many there were. It consumes the iterator.",
                example: "print(Iter.range(1, 5):count())",
            },
            Member {
                name: "any",
                kind: MemberKind::Method,
                signature: "Iter:any(f: (T) -> boolean): boolean",
                doc: "Whether `f` holds for some value. It stops at the first hit.",
                example: "print(Iter.from([ 1, 5 ]):any(function(n) return n > 3 end))",
            },
            Member {
                name: "all",
                kind: MemberKind::Method,
                signature: "Iter:all(f: (T) -> boolean): boolean",
                doc: "Whether `f` holds for every value. It stops at the first miss.",
                example: "print(Iter.from([ 1, 5 ]):all(function(n) return n > 0 end))",
            },
            Member {
                name: "find",
                kind: MemberKind::Method,
                signature: "Iter:find(f: (T) -> boolean): T?",
                doc: "The first value where `f` holds, or nil. It stops there, so the rest is never pulled.",
                example: "print(Iter.range(1, 10):find(function(n) return n % 4 == 0 end))",
            },
            Member {
                name: "reduce",
                kind: MemberKind::Method,
                signature: "Iter:reduce<U>(f: (U, T) -> U, init: U): U",
                doc: "Folds the run: `acc = f(acc, value)` from `init`, and the last `acc` is the result.",
                example: "local sum = Iter.range(1, 4):reduce(function(acc: number, n) return acc + n end, 0)\nprint(sum)",
            },
            Member {
                name: "first",
                kind: MemberKind::Method,
                signature: "Iter:first(): T?",
                doc: "The first value, or nil for an empty run. It pulls once.",
                example: "print(Iter.from([ 1, 2 ]):first())",
            },
            Member {
                name: "last",
                kind: MemberKind::Method,
                signature: "Iter:last(): T?",
                doc: "The last value, or nil for an empty run. It pulls the whole run.",
                example: "print(Iter.from([ 1, 2 ]):last())",
            },
        ],
    ),
    (
        "Symbol",
        &[Member {
            name: "new",
            kind: MemberKind::Static,
            signature: "Symbol.new(name: string)",
            doc: "A unique frozen table that no string can collide with. It prints as `Symbol(name)`, and `name` is a label alone: two symbols of one name are still two keys.",
            example: "local key = Symbol.new(\"slot\")\nlocal t = { [key] = 1 }\nprint(t[key])",
        }],
    ),
    (
        "Signal",
        &[
            Member {
                name: "new",
                kind: MemberKind::Static,
                signature: "Signal.new<T...>(): Signal<T...>",
                doc: "A signal that fires `T...`. The pack takes no explicit type arguments at the call, so write them with `<<...>>` or on the binding.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:Fire(10)",
            },
            Member {
                name: "is",
                kind: MemberKind::Static,
                signature: "Signal.is(value: any): boolean",
                doc: "Whether `value` is one of the std's signals. An `RBXScriptSignal` is not.",
                example: "local damaged = Signal.new<<number>>()\nprint(Signal.is(damaged))",
            },
            Member {
                name: "wrap",
                kind: MemberKind::Static,
                signature: "Signal.wrap<T...>(source: Signalish<T...>): Signal<T...>",
                doc: "A std Signal that fires whenever `source` does. `source` is anything with `Connect` or `connect`, a Roblox signal included.",
                example: "local source = Signal.new<<number>>()\nlocal mine = Signal.wrap(source)\nprint(Signal.is(mine))",
            },
            Member {
                name: "collect",
                kind: MemberKind::Static,
                signature: "Signal.collect<T...>(source: Signalish<T...>): ((...any) -> T..., SignalConnection)",
                doc: "An iterator that drains the queued events of `source` in order, and the connection that feeds it. Disconnect it when the loop ends.",
                example: "local changed = Signal.new<<number>>()\nlocal events, conn = Signal.collect(changed)\nchanged:Fire(1)\nprint(events(), conn.Connected)",
            },
            Member {
                name: "Connect",
                kind: MemberKind::Method,
                signature: "Signal:Connect(handler: (T...) -> ()): SignalConnection",
                doc: "Runs `handler` on every fire and returns the connection. Handlers run in connection order, each on a reused thread.",
                example: "local damaged = Signal.new<<number>>()\nlocal conn = damaged:Connect(function(amount) print(amount) end)\nconn:Disconnect()",
            },
            Member {
                name: "Once",
                kind: MemberKind::Method,
                signature: "Signal:Once(handler: (T...) -> ()): SignalConnection",
                doc: "Runs `handler` on the next fire alone, then disconnects itself.",
                example: "local ready = Signal.new<<>>()\nready:Once(function() print(\"go\") end)",
            },
            Member {
                name: "Wait",
                kind: MemberKind::Method,
                signature: "Signal:Wait(timeout: number?): T...",
                doc: "Yields until the next fire and returns its values. With a timeout it returns nothing when the time runs out.",
                example: "local damaged = Signal.new<<number>>()\nlocal amount = damaged:Wait(5)\nprint(amount)",
            },
            Member {
                name: "Fire",
                kind: MemberKind::Method,
                signature: "Signal:Fire(...: T...)",
                doc: "Runs every handler now, in connection order. A handler may disconnect any connection during the fire.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:Fire(10)",
            },
            Member {
                name: "FireDeferred",
                kind: MemberKind::Method,
                signature: "Signal:FireDeferred(...: T...)",
                doc: "Runs the handlers at the next resumption point, through `task.defer`, so the caller returns first.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:FireDeferred(10)",
            },
            Member {
                name: "DisconnectAll",
                kind: MemberKind::Method,
                signature: "Signal:DisconnectAll()",
                doc: "Drops every connection. The signal stays usable.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:DisconnectAll()",
            },
            Member {
                name: "Destroy",
                kind: MemberKind::Method,
                signature: "Signal:Destroy()",
                doc: "Disconnects everything and marks the signal dead, which is what `delete` calls.",
                example: "local damaged = Signal.new<<number>>()\ndelete damaged",
            },
            Member {
                name: "connect",
                kind: MemberKind::Method,
                signature: "Signal:connect(handler: (T...) -> ()): SignalConnection",
                doc: "The snake_case twin of `Connect`. Both name one function, for a file that keeps to Alloy's own naming.",
                example: "local damaged = Signal.new<<number>>()\nlocal conn = damaged:connect(function(amount) print(amount) end)\nconn:disconnect()",
            },
            Member {
                name: "once",
                kind: MemberKind::Method,
                signature: "Signal:once(handler: (T...) -> ()): SignalConnection",
                doc: "The snake_case twin of `Once`.",
                example: "local ready = Signal.new<<>>()\nready:once(function() print(\"go\") end)",
            },
            Member {
                name: "wait",
                kind: MemberKind::Method,
                signature: "Signal:wait(timeout: number?): T...",
                doc: "The snake_case twin of `Wait`.",
                example: "local damaged = Signal.new<<number>>()\nprint(damaged:wait(5))",
            },
            Member {
                name: "fire",
                kind: MemberKind::Method,
                signature: "Signal:fire(...: T...)",
                doc: "The snake_case twin of `Fire`.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:fire(10)",
            },
            Member {
                name: "fire_deferred",
                kind: MemberKind::Method,
                signature: "Signal:fire_deferred(...: T...)",
                doc: "The snake_case twin of `FireDeferred`.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:fire_deferred(10)",
            },
            Member {
                name: "disconnect_all",
                kind: MemberKind::Method,
                signature: "Signal:disconnect_all()",
                doc: "The snake_case twin of `DisconnectAll`.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:disconnect_all()",
            },
            Member {
                name: "destroy",
                kind: MemberKind::Method,
                signature: "Signal:destroy()",
                doc: "The snake_case twin of `Destroy`.",
                example: "local damaged = Signal.new<<number>>()\ndamaged:destroy()",
            },
        ],
    ),
    (
        "Attributes",
        &[
            Member {
                name: "get",
                kind: MemberKind::Static,
                signature: "Attributes.get<T>(target: any, attr: Attribute<T>): T?",
                doc: "The value an attribute carries on a function, a struct, or an enum, or nil when it carries none. `attr` is the declared name, not a string.",
                example: "attribute icon(asset: string) on struct\n@icon(\"rbxassetid://1\")\nstruct Sword as damage: number end\nprint(Attributes.get(Sword, icon))",
            },
            Member {
                name: "field",
                kind: MemberKind::Static,
                signature: "Attributes.field<T>(struct: any, field: string, attr: Attribute<T>): T?",
                doc: "The attribute's value on one field of a struct, by the field's name.",
                example: "attribute label(text: string) on field\nstruct Sword as\n    @label(\"Damage\") damage: number\nend\nprint(Attributes.field(Sword, \"damage\", label))",
            },
            Member {
                name: "variant",
                kind: MemberKind::Static,
                signature: "Attributes.variant<T>(enum: any, variant: string, attr: Attribute<T>): T?",
                doc: "The attribute's value on one variant of an enum, by the variant's name.",
                example: "attribute weight(n: number) on variant\nenum Drop as\n    @weight(3) Coin\nend\nprint(Attributes.variant(Drop, \"Coin\", weight))",
            },
            Member {
                name: "fields",
                kind: MemberKind::Static,
                signature: "Attributes.fields<T>(struct: any, attr: Attribute<T>): { [string]: T }",
                doc: "Every field of a struct that carries the attribute, as a table of field name to value.",
                example: "attribute label(text: string) on field\nstruct Sword as\n    @label(\"Damage\") damage: number\nend\nprint(Attributes.fields(Sword, label).damage)",
            },
            Member {
                name: "of",
                kind: MemberKind::Static,
                signature: "Attributes.of(target: any): any",
                doc: "Everything declared on the target, as the compiler wrote it. Use it to walk attributes no name is known for.",
                example: "attribute icon(asset: string) on struct\n@icon(\"rbxassetid://1\")\nstruct Sword as damage: number end\nprint(Attributes.of(Sword) ~= nil)",
            },
        ],
    ),
    (
        "Traits",
        &[
            Member {
                name: "Display",
                kind: MemberKind::Field,
                signature: "Display = { to_string(self): string }",
                doc: "How a value prints. An `impl Display` writes `to_string` and the emit sets `__tostring`, so `print(v)` and an interpolation use it.",
                example: "function show<T: Display>(v: T): string\n    return v:to_string()\nend",
            },
            Member {
                name: "Debug",
                kind: MemberKind::Field,
                signature: "Debug = { debug(self): string }",
                doc: "The developer view of a value, what `@derive(Debug)` writes and `$dbg` prints.",
                example: "function trace<T: Debug>(v: T): string\n    return v:debug()\nend",
            },
            Member {
                name: "Clone",
                kind: MemberKind::Field,
                signature: "Clone<T> = { clone(self): T }",
                doc: "A copy of a value, what `@derive(Clone)` writes.",
                example: "function twice<T: Clone>(v: T)\n    return v:clone()\nend",
            },
            Member {
                name: "Eq",
                kind: MemberKind::Field,
                signature: "Eq = { eq(self, other): boolean }",
                doc: "Equality. An `impl Eq` sets `__eq`, so `a == b` runs it.",
                example: "function same<T: Eq>(a: T, b: T): boolean\n    return a:eq(b)\nend",
            },
            Member {
                name: "PartialEq",
                kind: MemberKind::Field,
                signature: "PartialEq = { eq(self, other): boolean }",
                doc: "The same shape as `Eq`, under the name Rust uses for equality that is not total.",
                example: "function same<T: PartialEq>(a: T, b: T): boolean\n    return a:eq(b)\nend",
            },
            Member {
                name: "Ord",
                kind: MemberKind::Field,
                signature: "Ord = { lt(self, other): boolean, le(self, other): boolean }",
                doc: "Ordering. An `impl Ord` sets `__lt` and `__le`, so `a < b` runs it, and `@derive(Ord)` writes both.",
                example: "function smallest<T: Ord>(a: T, b: T): T\n    return a:lt(b) ? a : b\nend",
            },
            Member {
                name: "Add",
                kind: MemberKind::Field,
                signature: "Add<T> = { add(self, other): T }",
                doc: "Addition. An `impl Add` sets `__add`, so `a + b` runs it.",
                example: "function sum<T: Add>(a: T, b: T)\n    return a:add(b)\nend",
            },
            Member {
                name: "Sub",
                kind: MemberKind::Field,
                signature: "Sub<T> = { sub(self, other): T }",
                doc: "Subtraction, through `__sub`.",
                example: "function diff<T: Sub>(a: T, b: T)\n    return a:sub(b)\nend",
            },
            Member {
                name: "Mul",
                kind: MemberKind::Field,
                signature: "Mul<T> = { mul(self, other): T }",
                doc: "Multiplication, through `__mul`.",
                example: "function scale<T: Mul>(a: T, b: T)\n    return a:mul(b)\nend",
            },
            Member {
                name: "Div",
                kind: MemberKind::Field,
                signature: "Div<T> = { div(self, other): T }",
                doc: "Division, through `__div`.",
                example: "function ratio<T: Div>(a: T, b: T)\n    return a:div(b)\nend",
            },
            Member {
                name: "Serialize",
                kind: MemberKind::Field,
                signature: "Serialize = { serialize(self): any }",
                doc: "A value as plain data. The bound asks for `serialize`, which `@derive(Serialize)` writes beside `to_table` and `from_table`, so a derived struct meets the bound.",
                example: "function to_data<T: Serialize>(v: T): any\n    return v:serialize()\nend",
            },
        ],
    ),
];

/// The signature of each std type, as Alloy writes it. A reference page
/// heads the type's section with it.
pub const TYPE_SIGNATURES: &[(&str, &str)] = &[
    ("HashMap", "HashMap<K, V>"),
    ("Set", "Set<T>"),
    ("Array", "Array<T>"),
    ("ReadArray", "ReadArray<T>"),
    ("WriteArray", "WriteArray<T>"),
    ("Future", "Future<T>"),
    ("Result", "Result<T, E>"),
    ("Ok", "Ok<T>(value: T): Result<T, any>"),
    ("Err", "Err<E>(error: E, trace: string?): Result<any, E>"),
    ("Queue", "Queue<T>"),
    ("Heap", "Heap<T>"),
    ("Scope", "Scope"),
    ("Iter", "Iter<T>"),
    ("R15Character", "R15Character"),
    ("R6Character", "R6Character"),
    ("Symbol", "Symbol"),
    ("Signal", "Signal<T...>"),
    ("Partial", "Partial<T>"),
    ("Readonly", "Readonly<T>"),
    ("Sink", "Sink<T>"),
    ("Attributes", "Attributes"),
];

/// The signature of one std type, or nothing when the entry names no
/// type of its own.
pub fn type_signature(key: &str) -> Option<&'static str> {
    TYPE_SIGNATURES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, s)| *s)
}

/// The members of one entry, or nothing when the entry has none.
pub fn members(key: &str) -> &'static [Member] {
    MEMBERS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, m)| *m)
        .unwrap_or(&[])
}

/// One member of one entry, by name.
pub fn member(key: &str, name: &str) -> Option<&'static Member> {
    members(key).iter().find(|m| m.name == name)
}

/// The std type of that name, when it documents members.
pub fn member_owner(name: &str) -> Option<&'static str> {
    MEMBERS.iter().find(|(k, _)| *k == name).map(|(k, _)| *k)
}

/// `HashMap.get` or `HashMap:get` split into the type and the member.
/// None when the text names no member of a std type.
pub fn split_member(topic: &str) -> Option<(&'static str, &'static Member)> {
    let (owner, name) = topic.split_once(['.', ':'])?;
    let (key, _) = MEMBERS.iter().find(|(k, _)| *k == owner)?;

    member(key, name).map(|m| (*key, m))
}

/// The names of an entry's members, in the order they are documented.
pub fn member_names(key: &str) -> Vec<&'static str> {
    members(key).iter().map(|m| m.name).collect()
}

/// One member as Markdown: the signature, the doc, and the example.
/// This is what a hover on the member shows.
pub fn member_markdown(m: &Member) -> String {
    format!(
        "```alloy\n{}\n```\n{}\n\n```alloy\n{}\n```",
        m.signature, m.doc, m.example
    )
}

/// The member spot at `offset`: the word under it, the byte of the `.`
/// or the `:` before it, and the receiver word before that. None when
/// the byte is not a member read.
pub fn member_spot(source: &str, offset: usize) -> Option<(&str, usize, &str)> {
    let bytes = source.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';

    if !bytes.get(offset).is_some_and(|b| word(*b)) {
        return None;
    }

    let mut start = offset;
    let mut end = offset;

    while start > 0 && word(bytes[start - 1]) {
        start -= 1;
    }

    while end < bytes.len() && word(bytes[end]) {
        end += 1;
    }

    if start == 0 || !matches!(bytes[start - 1], b'.' | b':') {
        return None;
    }

    let sigil = start - 1;
    let head = &source[..sigil];
    let from = head
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0);

    Some((&source[start..end], sigil, &head[from..]))
}

/// The std type an annotation names: `HashMap<string, number>` is a
/// HashMap, `read number[]` a ReadArray, `Job[]` an Array.
pub fn type_head(annotation: &str) -> Option<String> {
    let t = annotation.trim().trim_end_matches('?').trim();

    if let Some(rest) = t.strip_prefix("read ") {
        return rest.trim().ends_with("[]").then(|| "ReadArray".to_string());
    }

    if let Some(rest) = t.strip_prefix("write ") {
        return rest
            .trim()
            .ends_with("[]")
            .then(|| "WriteArray".to_string());
    }

    if t.ends_with("[]") {
        return Some("Array".to_string());
    }

    let name = t.split('<').next().unwrap_or(t).trim();

    (!name.is_empty()).then(|| name.to_string())
}

/// The std type an initializer starts from: `HashMap.new()` is a
/// HashMap, `[ 1, 2 ]` an Array, `$set[ ]` a Set, `$map[ ]` a HashMap.
pub fn value_head(init: &str) -> Option<String> {
    let v = init.trim();

    if v.starts_with('[') {
        return Some("Array".to_string());
    }

    if v.starts_with("$set[") {
        return Some("Set".to_string());
    }

    if v.starts_with("$map[") {
        return Some("HashMap".to_string());
    }

    let name = v.split(['.', ':', '(']).next()?.trim();

    (!name.is_empty()).then(|| name.to_string())
}

/// Whether a member of that kind hangs off the type itself. A static
/// does; a method hangs off a value; a plain member reads on either.
pub fn member_fits(kind: MemberKind, on_type: bool) -> bool {
    match kind {
        MemberKind::Static => on_type,
        MemberKind::Method => !on_type,
        _ => true,
    }
}

/// A member as a hover shows it under the type the checker printed:
/// the name, what it does, and an example. The signature is the line
/// the checker prints above it.
pub fn member_hover(owner: &str, m: &Member) -> String {
    let sigil = match m.kind {
        MemberKind::Method => ":",
        _ => ".",
    };

    format!(
        "**{owner}{sigil}{}**\n\n{}\n\n```alloy\n{}\n```",
        m.name, m.doc, m.example
    )
}

/// A std type's overview with a line that names its members, which is
/// what a hover on the type name shows.
pub fn type_markdown(key: &str) -> Option<String> {
    let text = lookup(key)?;
    let names = member_names(key);

    if names.is_empty() {
        return Some(text.to_string());
    }

    let list: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();

    Some(format!("{text}\n\nMembers: {}", list.join(", ")))
}
