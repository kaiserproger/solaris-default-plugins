--!strict

local config: any = solaris.config()
local storage_key = "towns-v1"
local towns: any = {}
local storage_version: any = nil
local loaded = false
local pending: any = nil
local pending_queries: any = {}
local invites: any = {}
local invite_timers: any = {}
local startup_zones: any = nil
local sequence = 0

assert(type(config.dimension) == "string" and string.match(config.dimension, "^[a-z0-9_.-]+:[a-z0-9_./-]+$") ~= nil)
for _, key in ipairs({ "minimum_y", "maximum_y", "maximum_towns", "maximum_members_per_town", "maximum_claims", "invite_expiry_ticks", "maximum_online_query" }) do
    assert(type(config[key]) == "number" and config[key] % 1 == 0)
end
assert(config.minimum_y <= config.maximum_y)
assert(config.maximum_towns >= 1 and config.maximum_towns <= 12)
assert(config.maximum_members_per_town >= 1 and config.maximum_members_per_town <= 8)
assert(config.maximum_claims >= 1 and config.maximum_claims <= 24)
assert(config.invite_expiry_ticks >= 20 and config.invite_expiry_ticks <= 72000)
assert(config.maximum_online_query >= 1 and config.maximum_online_query <= 256)

local function normalize_uuid(value: string): string?
    local without_hyphens = string.gsub(value, "-", "")
    local normalized = string.lower(without_hyphens)
    if #normalized ~= 32 or string.match(normalized, "^[0-9a-f]+$") == nil then return nil end
    return normalized
end
local function valid_name(value: string): boolean return #value >= 2 and #value <= 16 and string.match(value, "^[a-z0-9_-]+$") ~= nil end
local function next_request(prefix: string): string sequence = sequence + 1 return prefix .. "-" .. tostring(sequence) end
local function chunk(value: number): number return math.floor(value / 16) end
local function claim_key(x: number, z: number): string return tostring(x) .. ":" .. tostring(z) end
local function coordinate_id(value: number): string return value < 0 and ("n" .. tostring(-value)) or ("p" .. tostring(value)) end
local function zone_id(town: any, x: number, z: number): string return "town-" .. town.name .. "-" .. coordinate_id(x) .. "-" .. coordinate_id(z) end

local function size(values: any): number local result = 0 for _ in pairs(values) do result = result + 1 end return result end
local function total_claims(values: any): number
    local result = 0
    for _, town in pairs(values) do result = result + size(town.claims) end
    return result
end
local function member_town(values: any, uuid: string): any
    for _, town in pairs(values) do if town.members[uuid] ~= nil then return town end end
    return nil
end
local function claim_owner(values: any, key: string): any
    for _, town in pairs(values) do if town.claims[key] ~= nil then return town end end
    return nil
end

local function copy_towns(source: any): any
    local copied: any = {}
    for name, town in pairs(source) do
        local members: any = {}
        local claims: any = {}
        for uuid, role in pairs(town.members) do members[uuid] = role end
        for key, value in pairs(town.claims) do claims[key] = { x = value.x, z = value.z } end
        copied[name] = { name = name, leader = town.leader, members = members, claims = claims }
    end
    return copied
end

local function encode(values: any): string
    local names: { string } = {}
    for name in pairs(values) do names[#names + 1] = name end
    table.sort(names)
    local rows: { string } = {}
    for _, name in ipairs(names) do
        local town = values[name]
        local member_ids: { string } = {}
        for uuid in pairs(town.members) do member_ids[#member_ids + 1] = uuid end
        table.sort(member_ids)
        local member_rows: { string } = {}
        for _, uuid in ipairs(member_ids) do member_rows[#member_rows + 1] = uuid .. ":" .. town.members[uuid] end
        local claim_ids: { string } = {}
        for key in pairs(town.claims) do claim_ids[#claim_ids + 1] = key end
        table.sort(claim_ids)
        rows[#rows + 1] = table.concat({ name, town.leader, table.concat(member_rows, "+"), table.concat(claim_ids, "+") }, ",")
    end
    return "v1|" .. table.concat(rows, ";")
end

local function decode(value: any): any
    local decoded: any = {}
    if value == nil or value == "v1|" then return decoded end
    if type(value) ~= "string" or string.sub(value, 1, 3) ~= "v1|" then return nil end
    for row in string.gmatch(string.sub(value, 4), "([^;]+)") do
        local name, leader_text, members_text, claims_text = string.match(row, "^([^,]+),([^,]+),([^,]*),([^,]*)$")
        local leader = normalize_uuid(leader_text or "")
        if name == nil or not valid_name(name) or leader == nil or decoded[name] ~= nil then return nil end
        local town = { name = name, leader = leader, members = {}, claims = {} }
        for member_row in string.gmatch(members_text or "", "([^+]+)") do
            local uuid_text, role = string.match(member_row, "^([0-9a-f]+):([a-z]+)$")
            local uuid = normalize_uuid(uuid_text or "")
            if uuid == nil or (role ~= "leader" and role ~= "officer" and role ~= "member") or town.members[uuid] ~= nil then return nil end
            town.members[uuid] = role
        end
        if town.members[leader] ~= "leader" or size(town.members) > config.maximum_members_per_town then return nil end
        for key in string.gmatch(claims_text or "", "([^+]+)") do
            local x_text, z_text = string.match(key, "^(-?%d+):(-?%d+)$")
            local x, z = tonumber(x_text), tonumber(z_text)
            if x == nil or z == nil or town.claims[key] ~= nil then return nil end
            town.claims[key] = { x = x, z = z }
        end
        decoded[name] = town
        if size(decoded) > config.maximum_towns or total_claims(decoded) > config.maximum_claims then return nil end
    end
    return decoded
end

local function register_claim(town: any, value: any)
    solaris.upsert_protected_zone(zone_id(town, value.x, value.z), config.dimension, town.leader,
        value.x * 16, config.minimum_y, value.z * 16,
        value.x * 16 + 15, config.maximum_y, value.z * 16 + 15)
end

local function save(player_id: number, next_towns: any, message: string, zone_action: string?, town_name: string?, value: any?)
    if pending ~= nil then solaris.send_message(player_id, "Another town update is committing; retry.") return end
    local revision = storage_version == nil and "new" or tostring(storage_version)
    local request_id = "save-v" .. revision
    pending = { request_id = request_id, player_id = player_id, before = towns, after = next_towns,
        message = message, zone_action = zone_action, town_name = town_name, value = value, stage = "save" }
    solaris.storage_cas(request_id, storage_key, storage_version, encode(next_towns))
end

function on_server_started(_event: any)
    solaris.storage_get(next_request("load"), storage_key)
end

function on_player_command(event: any)
    if event.root ~= "town" then return end
    if not loaded then solaris.send_message(event.player_id, "Towns are still loading.") return end
    local words: { string } = {}
    for word in string.gmatch(event.arguments, "%S+") do words[#words + 1] = word end
    local uuid = normalize_uuid(event.uuid)
    if uuid == nil then return end
    local own = member_town(towns, uuid)
    local action = string.lower(words[1] or "info")

    if action == "info" and #words <= 2 then
        local town = words[2] and towns[string.lower(words[2])] or own
        if town == nil then solaris.send_message(event.player_id, "Town not found.") return end
        solaris.send_message(event.player_id, town.name .. ": " .. tostring(size(town.members)) .. " members, " .. tostring(size(town.claims)) .. " claims.")
    elseif action == "create" and #words == 2 then
        local name = string.lower(words[2])
        if own ~= nil or not valid_name(name) or towns[name] ~= nil or size(towns) >= config.maximum_towns then
            solaris.send_message(event.player_id, "Cannot create that town.") return
        end
        local next_towns = copy_towns(towns)
        next_towns[name] = { name = name, leader = uuid, members = { [uuid] = "leader" }, claims = {} }
        save(event.player_id, next_towns, "Town " .. name .. " created.", nil, nil, nil)
    elseif action == "invite" and #words == 2 then
        if own == nil or (own.members[uuid] ~= "leader" and own.members[uuid] ~= "officer") then
            solaris.send_message(event.player_id, "Only a leader or officer can invite.") return
        end
        if size(own.members) >= config.maximum_members_per_town then solaris.send_message(event.player_id, "Town member limit reached.") return end
        local request_id = next_request("invite")
        pending_queries[request_id] = { player_id = event.player_id, town = own.name, target_name = string.lower(words[2]), inviter = event.username }
        solaris.list_online_players(request_id, config.maximum_online_query)
    elseif action == "join" and #words == 2 then
        local name = string.lower(words[2])
        if own ~= nil or invites[uuid] ~= name or towns[name] == nil then solaris.send_message(event.player_id, "No matching invitation.") return end
        local next_towns = copy_towns(towns)
        if size(next_towns[name].members) >= config.maximum_members_per_town then solaris.send_message(event.player_id, "Town member limit reached.") return end
        next_towns[name].members[uuid] = "member"
        invites[uuid] = nil
        save(event.player_id, next_towns, "Joined " .. name .. ".", nil, nil, nil)
    elseif action == "leave" and #words == 1 then
        if own == nil then solaris.send_message(event.player_id, "You are not in a town.") return end
        if own.leader == uuid then solaris.send_message(event.player_id, "A leader cannot leave; leadership transfer is not in this alpha.") return end
        local next_towns = copy_towns(towns)
        next_towns[own.name].members[uuid] = nil
        save(event.player_id, next_towns, "Left " .. own.name .. ".", nil, nil, nil)
    elseif action == "role" and #words == 3 then
        if own == nil or own.leader ~= uuid then solaris.send_message(event.player_id, "Only the leader can set roles.") return end
        local target = normalize_uuid(words[2])
        local role = string.lower(words[3])
        if target == nil or target == own.leader or own.members[target] == nil or (role ~= "member" and role ~= "officer") then
            solaris.send_message(event.player_id, "Use a member UUID and role member/officer.") return
        end
        local next_towns = copy_towns(towns)
        next_towns[own.name].members[target] = role
        save(event.player_id, next_towns, "Role updated.", nil, nil, nil)
    elseif (action == "claim" or action == "unclaim") and #words == 1 then
        if own == nil or own.leader ~= uuid then solaris.send_message(event.player_id, "Only the leader can manage claims.") return end
        local x, z = chunk(event.x), chunk(event.z)
        local key = claim_key(x, z)
        local current_owner = claim_owner(towns, key)
        if action == "claim" then
            if current_owner ~= nil or total_claims(towns) >= config.maximum_claims then solaris.send_message(event.player_id, "Chunk is claimed or the claim limit is reached.") return end
            local next_towns = copy_towns(towns)
            local value = { x = x, z = z }
            next_towns[own.name].claims[key] = value
            save(event.player_id, next_towns, "Chunk claimed for " .. own.name .. ".", "add", own.name, value)
        else
            if current_owner == nil or current_owner.name ~= own.name then solaris.send_message(event.player_id, "Your town does not claim this chunk.") return end
            local next_towns = copy_towns(towns)
            local value = next_towns[own.name].claims[key]
            next_towns[own.name].claims[key] = nil
            save(event.player_id, next_towns, "Chunk unclaimed.", "remove", own.name, value)
        end
    else
        solaris.send_message(event.player_id, "Usage: /town <info|create|invite|join|leave|role|claim|unclaim>")
    end
end

function on_player_online_result(event: any)
    local current = pending_queries[event.request_id]
    if current == nil then return end
    pending_queries[event.request_id] = nil
    local found: any = nil
    for _, player in ipairs(event.players) do
        if string.lower(player.username) == current.target_name then
            if found ~= nil then found = nil break end
            found = player
        end
    end
    if found == nil or member_town(towns, normalize_uuid(found.uuid) or "") ~= nil then
        solaris.send_message(current.player_id, "Online unclaimed player not found or ambiguous.") return
    end
    local target_uuid = normalize_uuid(found.uuid)
    if target_uuid == nil then return end
    invites[target_uuid] = current.town
    local timer_id = "invite-" .. tostring(found.player_id)
    invite_timers[timer_id] = target_uuid
    solaris.schedule_timer(timer_id, config.invite_expiry_ticks)
    solaris.send_message(found.player_id, current.inviter .. " invited you to " .. current.town .. ". Use /town join " .. current.town .. ".")
    solaris.send_message(current.player_id, "Invitation sent.")
end

function on_plugin_timer(event: any)
    local uuid = invite_timers[event.timer_id]
    if uuid ~= nil then invites[uuid] = nil invite_timers[event.timer_id] = nil end
end

function on_plugin_storage_get_result(event: any)
    if event.key ~= storage_key or event.failure ~= nil then loaded = false return end
    local decoded = decode(event.value)
    if decoded == nil then loaded = false return end
    towns = decoded
    storage_version = event.version
    local remaining: any = {}
    for _, town in pairs(towns) do
        for _, value in pairs(town.claims) do
            remaining[zone_id(town, value.x, value.z)] = true
            register_claim(town, value)
        end
    end
    if next(remaining) == nil then loaded = true else startup_zones = { remaining = remaining, failed = false } end
end

function on_plugin_storage_cas_result(event: any)
    local current = pending
    if current == nil or current.request_id ~= event.request_id then return end
    if current.stage == "rollback" then
        pending = nil
        if event.failure == nil and event.applied then towns = current.before storage_version = event.version loaded = true else loaded = false end
        solaris.send_message(current.player_id, "Claim protection failed; the town change was rolled back.")
        return
    end
    if event.failure ~= nil or not event.applied then
        pending = nil
        solaris.send_message(current.player_id, "Town data changed concurrently; retry.")
        return
    end
    towns = current.after
    storage_version = event.version
    if current.zone_action == nil then
        pending = nil
        solaris.send_message(current.player_id, current.message)
    else
        current.stage = "zone"
        local town = towns[current.town_name]
        if current.zone_action == "add" then register_claim(town, current.value) else solaris.remove_zone(zone_id(town, current.value.x, current.value.z)) end
    end
end

function on_zone_command_result(event: any)
    if startup_zones ~= nil and startup_zones.remaining[event.zone_id] then
        startup_zones.remaining[event.zone_id] = nil
        if not event.accepted then startup_zones.failed = true end
        if next(startup_zones.remaining) == nil then loaded = not startup_zones.failed startup_zones = nil end
        return
    end
    local current = pending
    if current == nil or current.stage ~= "zone" then return end
    local town = towns[current.town_name]
    if zone_id(town, current.value.x, current.value.z) ~= event.zone_id then return end
    if event.accepted then
        pending = nil
        solaris.send_message(current.player_id, current.message)
    else
        local request_id = "rollback-v" .. tostring(storage_version)
        current.request_id = request_id
        current.stage = "rollback"
        solaris.storage_cas(request_id, storage_key, storage_version, encode(current.before))
    end
end

function on_command_batch_rejected(_result: any)
    if pending ~= nil and pending.stage == "save" then pending = nil end
end
