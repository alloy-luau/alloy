// Luau's analyzer and VM for the playground, as one wasm module. The
// page hands it the check artifact and the runtime; it answers with the
// type errors, the completions, and the type at a position, and runs
// the ship artifact with `print` captured.
#include "Luau/AstQuery.h"
#include "Luau/Autocomplete.h"
#include "Luau/BuiltinDefinitions.h"
#include "Luau/Frontend.h"
#include "Luau/Module.h"
#include "Luau/ToString.h"

#include "lua.h"
#include "luacode.h"
#include "lualib.h"

#include <cstring>
#include <memory>
#include <string>
#include <unordered_map>

namespace
{

struct Sources : Luau::FileResolver
{
    std::unordered_map<Luau::ModuleName, std::string> source;

    std::optional<Luau::SourceCode> readSource(const Luau::ModuleName& name) override
    {
        auto it = source.find(name);
        if (it == source.end())
            return std::nullopt;
        return Luau::SourceCode{it->second, Luau::SourceCode::Module};
    }

    // `require("@alloy")` and `require("./alloy")` name the runtime.
    std::optional<Luau::ModuleInfo> resolveModule(const Luau::ModuleInfo*, Luau::AstExpr* node, const Luau::TypeCheckLimits&) override
    {
        if (Luau::AstExprConstantString* expr = node->as<Luau::AstExprConstantString>())
        {
            std::string path{expr->value.data, expr->value.size};
            if (path == "@alloy" || path == "./alloy")
                return Luau::ModuleInfo{"alloy"};
            if (source.count(path))
                return Luau::ModuleInfo{path};
        }
        return std::nullopt;
    }

    std::string getHumanReadableModuleName(const Luau::ModuleName& name) const override
    {
        return name;
    }
};

struct Strict : Luau::ConfigResolver
{
    Luau::Config config;

    Strict()
    {
        config.mode = Luau::Mode::Strict;
    }

    const Luau::Config& getConfig(const Luau::ModuleName&, const Luau::TypeCheckLimits&) const override
    {
        return config;
    }
};

Sources* sources = nullptr;
Strict* strict = nullptr;
Luau::Frontend* frontend = nullptr;
std::string out;

std::string json_string(const std::string& s)
{
    std::string r = "\"";
    for (unsigned char c : s)
    {
        switch (c)
        {
        case '"': r += "\\\""; break;
        case '\\': r += "\\\\"; break;
        case '\n': r += "\\n"; break;
        case '\r': r += "\\r"; break;
        case '\t': r += "\\t"; break;
        default:
            if (c < 0x20)
            {
                char buf[8];
                snprintf(buf, sizeof(buf), "\\u%04x", c);
                r += buf;
            }
            else
                r += (char)c;
        }
    }
    return r + "\"";
}

Luau::ToStringOptions type_options()
{
    Luau::ToStringOptions opts;
    opts.exhaustive = true;
    opts.useLineBreaks = true;
    opts.maxTypeLength = 20000;
    opts.maxTableLength = 20000;
    return opts;
}

const char* kind_name(Luau::AutocompleteEntryKind kind)
{
    switch (kind)
    {
    case Luau::AutocompleteEntryKind::Property: return "property";
    case Luau::AutocompleteEntryKind::Binding: return "variable";
    case Luau::AutocompleteEntryKind::Keyword: return "keyword";
    case Luau::AutocompleteEntryKind::String: return "string";
    case Luau::AutocompleteEntryKind::Type: return "type";
    case Luau::AutocompleteEntryKind::Module: return "module";
    case Luau::AutocompleteEntryKind::GeneratedFunction: return "function";
    case Luau::AutocompleteEntryKind::RequirePath: return "module";
    default: return "text";
    }
}

const char* context_name(Luau::AutocompleteContext ctx)
{
    switch (ctx)
    {
    case Luau::AutocompleteContext::Expression: return "expression";
    case Luau::AutocompleteContext::Statement: return "statement";
    case Luau::AutocompleteContext::Property: return "property";
    case Luau::AutocompleteContext::Type: return "type";
    case Luau::AutocompleteContext::Keyword: return "keyword";
    case Luau::AutocompleteContext::String: return "string";
    default: return "unknown";
    }
}

void ensure_checked()
{
    frontend->check("main");
}

} // namespace

extern "C" const char* alloy_init(const char* definitions)
{
    out.clear();
    try
    {
        // Every Luau feature flag on, as luau-lsp and luau.org run: the
        // new solver leans on its companions, and the definitions do
        // not check without them.
        for (Luau::FValue<bool>* flag = Luau::FValue<bool>::list; flag; flag = flag->next)
            if (strncmp(flag->name, "Luau", 4) == 0)
                flag->value = true;

        // The definitions are one large file: the solver's recursion
        // and iteration limits, set for a module, get room.
        for (Luau::FValue<int>* flag = Luau::FValue<int>::list; flag; flag = flag->next)
            if (strncmp(flag->name, "Luau", 4) == 0 && strstr(flag->name, "Limit") != nullptr && flag->value > 0 && flag->value < 100000000)
                flag->value = flag->value * 8;

        sources = new Sources();
        strict = new Strict();
        Luau::FrontendOptions options;
        options.retainFullTypeGraphs = true;
        frontend = new Luau::Frontend(Luau::SolverMode::New, sources, strict, options);

        Luau::unfreeze(frontend->globals.globalTypes);
        Luau::registerBuiltinGlobals(*frontend, frontend->globals);

        if (definitions && *definitions)
        {
            Luau::LoadDefinitionFileResult result =
                frontend->loadDefinitionFile(frontend->globals, frontend->globals.globalScope, definitions, "@roblox", false, false);
            if (!result.success)
            {
                out = "definitions: ";
                if (!result.parseResult.errors.empty())
                    out += result.parseResult.errors[0].getMessage();
                else if (result.module && !result.module->errors.empty())
                    out += Luau::toString(result.module->errors[0]) + " at line " + std::to_string(result.module->errors[0].location.begin.line + 1);
                else
                    out += "failed to load";
            }
        }

        Luau::freeze(frontend->globals.globalTypes);
    }
    catch (const std::exception& e)
    {
        out = std::string("init: ") + e.what();
    }
    return out.c_str();
}

extern "C" void alloy_set_module(const char* name, const char* source)
{
    if (!frontend)
        return;
    sources->source[name] = source;
    frontend->markDirty(name);
}

extern "C" const char* alloy_check()
{
    out = "[";
    try
    {
        Luau::CheckResult result = frontend->check("main");
        bool first = true;
        for (const Luau::TypeError& err : result.errors)
        {
            if (err.moduleName != "main")
                continue;
            if (!first)
                out += ",";
            first = false;
            out += "{\"line\":" + std::to_string(err.location.begin.line) + ",\"col\":" + std::to_string(err.location.begin.column) +
                   ",\"endLine\":" + std::to_string(err.location.end.line) + ",\"endCol\":" + std::to_string(err.location.end.column) +
                   ",\"message\":" + json_string(Luau::toString(err)) + "}";
        }
    }
    catch (const std::exception& e)
    {
        out = "[{\"line\":0,\"col\":0,\"endLine\":0,\"endCol\":1,\"message\":" + json_string(std::string("analyzer: ") + e.what()) + "}";
    }
    out += "]";
    return out.c_str();
}

extern "C" const char* alloy_autocomplete(int line, int col)
{
    out = "{\"context\":\"unknown\",\"items\":[]}";
    try
    {
        ensure_checked();
        Luau::AutocompleteResult result = Luau::autocomplete(
            *frontend,
            "main",
            Luau::Position{(unsigned)line, (unsigned)col},
            [](std::string, std::optional<const Luau::ExternType*>, std::optional<std::string>) -> std::optional<Luau::AutocompleteEntryMap>
            {
                return std::nullopt;
            }
        );
        Luau::ToStringOptions opts;
        opts.maxTypeLength = 20000;
        opts.maxTableLength = 40;
        out = std::string("{\"context\":\"") + context_name(result.context) + "\",\"items\":[";
        bool first = true;
        for (const auto& [name, entry] : result.entryMap)
        {
            if (!first)
                out += ",";
            first = false;
            out += "{\"label\":" + json_string(name) + ",\"kind\":\"" + kind_name(entry.kind) + "\"";
            if (entry.type)
                out += ",\"type\":" + json_string(Luau::toString(*entry.type, opts));
            if (entry.deprecated)
                out += ",\"deprecated\":true";
            if (entry.insertText)
                out += ",\"insert\":" + json_string(*entry.insertText);
            if (entry.documentationSymbol)
                out += ",\"symbol\":" + json_string(*entry.documentationSymbol);
            out += "}";
        }
        out += "]}";
    }
    catch (const std::exception&)
    {
    }
    return out.c_str();
}

extern "C" const char* alloy_hover(int line, int col)
{
    out = "null";
    try
    {
        ensure_checked();
        const Luau::SourceModule* sm = frontend->getSourceModule("main");
        Luau::ModulePtr module = frontend->moduleResolver.getModule("main");
        if (!sm || !module)
            return out.c_str();
        Luau::Position pos{(unsigned)line, (unsigned)col};
        std::optional<Luau::TypeId> ty = Luau::findTypeAtPosition(*module, *sm, pos);
        std::string name;
        std::string kind = "expression";
        // A name where it is declared is a local, not an expression: the
        // scope at the position knows its type.
        if (!ty)
        {
            Luau::ExprOrLocal target = Luau::findExprOrLocalAtPosition(*sm, pos);
            if (Luau::AstLocal* local = target.getLocal())
            {
                if (Luau::ScopePtr scope = Luau::findScopeAtPosition(*module, pos))
                    ty = scope->lookup(local);
                name = local->name.value;
                kind = "local";
            }
            if (!ty)
                return out.c_str();
        }
        if (Luau::AstExpr* expr = Luau::findExprAtPosition(*sm, pos))
        {
            if (Luau::AstExprLocal* l = expr->as<Luau::AstExprLocal>())
            {
                name = l->local->name.value;
                kind = "local";
            }
            else if (Luau::AstExprGlobal* g = expr->as<Luau::AstExprGlobal>())
            {
                name = g->name.value;
                kind = "global";
            }
            else if (Luau::AstExprIndexName* i = expr->as<Luau::AstExprIndexName>())
            {
                name = i->index.value;
                kind = "property";
            }
        }
        if (name.empty())
        {
            if (std::optional<Luau::Binding> binding = Luau::findBindingAtPosition(*module, *sm, pos))
            {
                ty = binding->typeId;
                kind = "local";
            }
        }
        Luau::ToStringOptions opts = type_options();
        out = "{\"name\":" + json_string(name) + ",\"kind\":\"" + kind + "\",\"type\":" + json_string(Luau::toString(*ty, opts)) + "}";
    }
    catch (const std::exception& e)
    {
        out = "{\"error\":" + json_string(e.what()) + "}";
    }
    return out.c_str();
}

namespace
{

std::string run_output;

int captured_print(lua_State* L)
{
    int n = lua_gettop(L);
    for (int i = 1; i <= n; i++)
    {
        size_t len;
        const char* s = luaL_tolstring(L, i, &len);
        if (i > 1)
            run_output += "\t";
        run_output.append(s, len);
        lua_pop(L, 1);
    }
    run_output += "\n";
    return 0;
}

// `require("@alloy")` gives the runtime, loaded once per run.
int playground_require(lua_State* L)
{
    const char* name = luaL_checkstring(L, 1);
    if (strcmp(name, "@alloy") != 0 && strcmp(name, "./alloy") != 0)
        luaL_error(L, "the playground has one module to require, \"@alloy\"; \"%s\" is not it", name);
    lua_getfield(L, LUA_REGISTRYINDEX, "alloy_runtime");
    if (!lua_isnil(L, -1))
        return 1;
    lua_pop(L, 1);
    lua_getfield(L, LUA_REGISTRYINDEX, "alloy_runtime_source");
    size_t len;
    const char* src = lua_tolstring(L, -1, &len);
    size_t bytecodeSize = 0;
    char* bytecode = luau_compile(src, len, nullptr, &bytecodeSize);
    int status = luau_load(L, "=alloy", bytecode, bytecodeSize, 0);
    free(bytecode);
    if (status != 0)
        lua_error(L);
    lua_call(L, 0, 1);
    lua_pushvalue(L, -1);
    lua_setfield(L, LUA_REGISTRYINDEX, "alloy_runtime");
    return 1;
}

} // namespace

extern "C" const char* alloy_run(const char* runtime, const char* source)
{
    run_output.clear();
    std::unique_ptr<lua_State, void (*)(lua_State*)> state(luaL_newstate(), lua_close);
    lua_State* L = state.get();
    luaL_openlibs(L);
    lua_pushcfunction(L, captured_print, "print");
    lua_setglobal(L, "print");
    lua_pushcfunction(L, playground_require, "require");
    lua_setglobal(L, "require");
    lua_pushstring(L, runtime);
    lua_setfield(L, LUA_REGISTRYINDEX, "alloy_runtime_source");
    luaL_sandbox(L);
    luaL_sandboxthread(L);

    size_t bytecodeSize = 0;
    char* bytecode = luau_compile(source, strlen(source), nullptr, &bytecodeSize);
    int status = luau_load(L, "=play", bytecode, bytecodeSize, 0);
    free(bytecode);
    if (status != 0)
    {
        size_t len;
        const char* msg = lua_tolstring(L, -1, &len);
        run_output += "error: ";
        run_output.append(msg, len);
        out = run_output;
        return out.c_str();
    }
    lua_State* T = lua_newthread(L);
    lua_pushvalue(L, -2);
    lua_xmove(L, T, 1);
    status = lua_resume(T, nullptr, 0);
    if (status != 0 && status != LUA_YIELD)
    {
        run_output += "error: ";
        if (const char* str = lua_tostring(T, -1))
            run_output += str;
        run_output += "\n";
        run_output += lua_debugtrace(T);
    }
    else if (status == LUA_YIELD)
    {
        run_output += "(the script yielded and the playground has no scheduler to resume it)\n";
    }
    out = run_output;
    return out.c_str();
}
