--!strict

local config: any = solaris.config()
local storage_key = "assignments-v1"
local groups: any = {}
local assignments: any = {}
local storage_version: any = nil
local loaded = false
local pending: any = nil
local sequence = 0

assert(type(config.default_group) == "string")
assert(type(config.maximum_assignments) == "number" and config.maximum_assignments % 1 == 0)
assert(config.maximum_assignments >= 1 and config.maximum_assignments <= 64)
assert(type(config.groups) == "table" and #config.groups >= 1 and #config.groups <= 16)
for _, group in ipairs(config.groups) do
    assert(type(group.name) == "string" and string.match(group.name, "^[a-z0-9_-]+$") ~= nil)
    assert(groups[group.name] == nil and type(group.nodes) == "table" and #group.nodes <= 64)
    local nodes: any = {}
    for _, node in ipairs(group.nodes) do
        assert(type(node) == "string" and #node >= 1 and #node <= 128)
        assert(string.match(node, "^[a-z0-9_.*-]+@?[a-z0-9_.:/-]*$") ~= nil)
        nodes[node] = true
    end
    groups[group.name] = nodes
end
assert(groups[config.default_group] ~= nil)

local function normalize_uuid(value: string): string?
    local without_hyphens = string.gsub(value, "-", "")
    local normalized = string.lower(without_hyphens)
    if #normalized ~= 32 or string.match(normalized, "^[0-9a-f]+$") == nil then return nil end
    return normalized
end

local function next_request(prefix: string): string
    sequence = sequence + 1
    return prefix .. "-" .. tostring(sequence)
end

local function count(values: any): number
    local result = 0
    for _ in pairs(values) do result = result + 1 end
    return result
end

local function copy_assignments(): any
    local copied: any = {}
    for uuid, group in pairs(assignments) do copied[uuid] = group end
    return copied
end

local function encode(values: any): string
    local ids: { string } = {}
    for uuid in pairs(values) do ids[#ids + 1] = uuid end
    table.sort(ids)
    local rows: { string } = {}
    for _, uuid in ipairs(ids) do rows[#rows + 1] = uuid .. "," .. values[uuid] end
    return "v1|" .. table.concat(rows, ";")
end

local function decode(value: any): any
    local decoded: any = {}
    if value == nil or value == "v1|" then return decoded end
    if type(value) ~= "string" or string.sub(value, 1, 3) ~= "v1|" then return nil end
    for row in string.gmatch(string.sub(value, 4), "([^;]+)") do
        local uuid_text, group = string.match(row, "^([0-9a-f]+),([a-z0-9_-]+)$")
        local uuid = normalize_uuid(uuid_text or "")
        if uuid == nil or groups[group] == nil or decoded[uuid] ~= nil then return nil end
        decoded[uuid] = group
        if count(decoded) > config.maximum_assignments then return nil end
    end
    return decoded
end

local function effective_group(uuid: string): string
    return assignments[uuid] or config.default_group
end

local function has_node(uuid: string, node: string, context: string?): boolean
    local nodes = groups[effective_group(uuid)]
    if nodes["*"] then return true end
    if context ~= nil and nodes[node .. "@" .. context] then return true end
    return nodes[node] == true
end

local function save(player_id: number, next_values: any, message: string)
    if pending ~= nil then
        solaris.send_message(player_id, "Another permission update is committing; retry.")
        return
    end
    local revision = storage_version == nil and "new" or tostring(storage_version)
    local request_id = "save-v" .. revision
    pending = { request_id = request_id, player_id = player_id, values = next_values, message = message }
    solaris.storage_cas(request_id, storage_key, storage_version, encode(next_values))
end

function on_server_started(_event: any)
    local request_id = next_request("load")
    solaris.storage_get(request_id, storage_key)
end

function on_player_command(event: any)
    if event.root ~= "perm" then return end
    if not loaded then
        solaris.send_message(event.player_id, "Permissions are still loading.")
        return
    end
    local words: { string } = {}
    for word in string.gmatch(event.arguments, "%S+") do words[#words + 1] = word end
    local actor = normalize_uuid(event.uuid)
    if actor == nil then return end

    if words[1] == nil or words[1] == "groups" then
        local names: { string } = {}
        for name in pairs(groups) do names[#names + 1] = name end
        table.sort(names)
        solaris.send_message(event.player_id, "Groups: " .. table.concat(names, ", "))
    elseif words[1] == "check" and words[2] ~= nil and #words <= 3 then
        local allowed = has_node(actor, string.lower(words[2]), words[3] and string.lower(words[3]) or nil)
        solaris.send_message(event.player_id, allowed and "Permission granted." or "Permission denied.")
    elseif words[1] == "user" and words[2] ~= nil and words[3] ~= nil then
        if not event.operator then
            solaris.send_message(event.player_id, "Only an operator can change groups.")
            return
        end
        local target = words[2] == "me" and actor or normalize_uuid(words[2])
        if target == nil then
            solaris.send_message(event.player_id, "Use a player UUID or me.")
            return
        end
        if words[3] == "list" and #words == 3 then
            solaris.send_message(event.player_id, target .. " is " .. effective_group(target) .. ".")
        elseif words[3] == "clear" and #words == 3 then
            local next_values = copy_assignments()
            next_values[target] = nil
            save(event.player_id, next_values, "Group reset to " .. config.default_group .. ".")
        elseif words[3] == "set" and words[4] ~= nil and #words == 4 then
            local group = string.lower(words[4])
            if groups[group] == nil then
                solaris.send_message(event.player_id, "Unknown group.")
                return
            end
            local next_values = copy_assignments()
            if next_values[target] == nil and count(next_values) >= config.maximum_assignments then
                solaris.send_message(event.player_id, "Assignment limit reached.")
                return
            end
            next_values[target] = group
            save(event.player_id, next_values, "Group set to " .. group .. ".")
        else
            solaris.send_message(event.player_id, "Usage: /perm user <uuid|me> <set group|clear|list>")
        end
    else
        solaris.send_message(event.player_id, "Usage: /perm <groups|check node [context]|user ...>")
    end
end

function on_plugin_storage_get_result(event: any)
    if event.key ~= storage_key then return end
    if event.failure ~= nil then loaded = false return end
    local decoded = decode(event.value)
    if decoded == nil then loaded = false return end
    assignments = decoded
    storage_version = event.version
    loaded = true
end

function on_plugin_storage_cas_result(event: any)
    local current = pending
    if current == nil or current.request_id ~= event.request_id then return end
    pending = nil
    if event.failure ~= nil or not event.applied then
        loaded = false
        solaris.send_message(current.player_id, "Permission update conflicted; reload required.")
        local request_id = next_request("reload")
        solaris.storage_get(request_id, storage_key)
        return
    end
    assignments = current.values
    storage_version = event.version
    solaris.send_message(current.player_id, current.message)
end

function on_command_batch_rejected(_result: any)
    pending = nil
end
