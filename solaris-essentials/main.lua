--!strict

local config: any = solaris.config()
local pending_storage: any = {}
local pending_saves: any = {}
local pending_queries: any = {}
local pending_teleports: any = {}
local tpa_by_target: any = {}
local replies: any = {}
local backs: any = {}
local sequence = 0

assert(type(config.maximum_homes_per_player) == "number" and config.maximum_homes_per_player % 1 == 0)
assert(config.maximum_homes_per_player >= 1 and config.maximum_homes_per_player <= 8)
assert(type(config.maximum_warps) == "number" and config.maximum_warps % 1 == 0)
assert(config.maximum_warps >= 1 and config.maximum_warps <= 24)
assert(type(config.tpa_expiry_ticks) == "number" and config.tpa_expiry_ticks % 1 == 0)
assert(config.tpa_expiry_ticks >= 20 and config.tpa_expiry_ticks <= 72000)
assert(type(config.maximum_online_query) == "number" and config.maximum_online_query % 1 == 0)
assert(config.maximum_online_query >= 1 and config.maximum_online_query <= 256)

local function normalize_uuid(value: string): string?
    local without_hyphens = string.gsub(value, "-", "")
    local normalized = string.lower(without_hyphens)
    if #normalized ~= 32 or string.match(normalized, "^[0-9a-f]+$") == nil then return nil end
    return normalized
end

local function valid_name(value: string): boolean
    return #value >= 1 and #value <= 24 and string.match(value, "^[a-z0-9_-]+$") ~= nil
end

local function next_request(prefix: string): string
    sequence = sequence + 1
    return prefix .. "-" .. tostring(sequence)
end

local function homes_key(uuid: string): string return "homes:" .. uuid end
local function position(x: number, y: number, z: number): any return { x = x, y = y, z = z } end

local function copy_positions(values: any): any
    local copied: any = {}
    for name, value in pairs(values) do copied[name] = position(value.x, value.y, value.z) end
    return copied
end

local function count(values: any): number
    local result = 0
    for _ in pairs(values) do result = result + 1 end
    return result
end

local function encode_positions(values: any): string
    local names: { string } = {}
    for name in pairs(values) do names[#names + 1] = name end
    table.sort(names)
    local rows: { string } = {}
    for _, name in ipairs(names) do
        local value = values[name]
        rows[#rows + 1] = table.concat({ name, tostring(value.x), tostring(value.y), tostring(value.z) }, ",")
    end
    return "v1|" .. table.concat(rows, ";")
end

local function decode_positions(value: any, maximum: number): any
    local decoded: any = {}
    if value == nil or value == "v1|" then return decoded end
    if type(value) ~= "string" or string.sub(value, 1, 3) ~= "v1|" then return nil end
    for row in string.gmatch(string.sub(value, 4), "([^;]+)") do
        local name, x_text, y_text, z_text = string.match(row, "^([a-z0-9_-]+),([^,]+),([^,]+),([^,]+)$")
        local x, y, z = tonumber(x_text), tonumber(y_text), tonumber(z_text)
        if name == nil or not valid_name(name) or x == nil or y == nil or z == nil
            or x ~= x or y ~= y or z ~= z or decoded[name] ~= nil then return nil end
        decoded[name] = position(x, y, z)
        if count(decoded) > maximum then return nil end
    end
    return decoded
end

local function read_positions(event: any, kind: string, key: string, name: string, value: any?)
    local request_id = next_request("read")
    pending_storage[request_id] = {
        player_id = event.player_id, uuid = event.uuid, key = key, kind = kind,
        name = name, value = value, old = position(event.x, event.y, event.z),
    }
    solaris.storage_get(request_id, key)
end

local function begin_teleport(player_id: number, target: any, old: any, label: string)
    local request_id = next_request("teleport")
    pending_teleports[request_id] = { player_id = player_id, old = old, label = label }
    solaris.teleport_player(request_id, player_id, target.x, target.y, target.z)
end

local function query(event: any, kind: string, target_name: string, message: string?)
    local request_id = next_request("online")
    pending_queries[request_id] = {
        kind = kind, player_id = event.player_id, username = event.username,
        target_name = string.lower(target_name), message = message,
        old = position(event.x, event.y, event.z), target_position = position(event.x, event.y, event.z),
    }
    solaris.list_online_players(request_id, config.maximum_online_query)
end

local function find_player(players: any, username: string): any
    local found: any = nil
    for _, player in ipairs(players) do
        if string.lower(player.username) == username then
            if found ~= nil then return nil end
            found = player
        end
    end
    return found
end

function on_player_command(event: any)
    local words: { string } = {}
    for word in string.gmatch(event.arguments, "%S+") do words[#words + 1] = word end
    local uuid = normalize_uuid(event.uuid)
    if uuid == nil then return end

    if event.root == "sethome" or event.root == "home" or event.root == "delhome" then
        local name = string.lower(words[1] or "home")
        if #words > 1 or not valid_name(name) then
            solaris.send_message(event.player_id, "Use one lowercase home name.")
            return
        end
        local kind = event.root == "sethome" and "home-set" or (event.root == "delhome" and "home-delete" or "home-get")
        read_positions(event, kind, homes_key(uuid), name, event.root == "sethome" and position(event.x, event.y, event.z) or nil)
        return
    end

    if event.root == "setwarp" or event.root == "delwarp" or event.root == "setspawn" then
        if not event.operator then
            solaris.send_message(event.player_id, "Only an operator can change warps.")
            return
        end
        local name = event.root == "setspawn" and "spawn" or string.lower(words[1] or "")
        if (event.root == "setspawn" and #words ~= 0) or not valid_name(name) or (event.root ~= "setspawn" and #words ~= 1) then
            solaris.send_message(event.player_id, "Usage: /setwarp <name>, /delwarp <name>, or /setspawn")
            return
        end
        local kind = event.root == "delwarp" and "warp-delete" or "warp-set"
        read_positions(event, kind, "warps-v1", name, kind == "warp-set" and position(event.x, event.y, event.z) or nil)
        return
    end

    if event.root == "warp" or event.root == "spawn" then
        local name = event.root == "spawn" and "spawn" or string.lower(words[1] or "")
        if not valid_name(name) or (event.root == "spawn" and #words ~= 0) or (event.root == "warp" and #words ~= 1) then
            solaris.send_message(event.player_id, "Usage: /warp <name> or /spawn")
            return
        end
        read_positions(event, "warp-get", "warps-v1", name, nil)
        return
    end

    if event.root == "back" then
        if #words ~= 0 or backs[event.player_id] == nil then
            solaris.send_message(event.player_id, "No back location is available.")
            return
        end
        begin_teleport(event.player_id, backs[event.player_id], position(event.x, event.y, event.z), "Back")
        return
    end

    if event.root == "tpa" then
        if #words ~= 1 then solaris.send_message(event.player_id, "Usage: /tpa <online-player>") return end
        query(event, "tpa", words[1], nil)
        return
    end

    if event.root == "tpaccept" then
        if #words ~= 0 then solaris.send_message(event.player_id, "Usage: /tpaccept") return end
        local request = tpa_by_target[event.player_id]
        if request == nil then solaris.send_message(event.player_id, "No TPA request is pending.") return end
        local request_id = next_request("online")
        pending_queries[request_id] = {
            kind = "tpa-accept", player_id = event.player_id, request = request,
            target_position = position(event.x, event.y, event.z),
        }
        solaris.list_online_players(request_id, config.maximum_online_query)
        return
    end

    if event.root == "msg" then
        local target, message = string.match(event.arguments, "^%s*(%S+)%s+(.+)%s*$")
        if target == nil or message == nil or #message > 256 then
            solaris.send_message(event.player_id, "Usage: /msg <online-player> <message>")
            return
        end
        query(event, "msg", target, message)
        return
    end

    if event.root == "reply" then
        local message = string.match(event.arguments, "^%s*(.-)%s*$") or ""
        local target_id = replies[event.player_id]
        if target_id == nil or message == "" or #message > 256 then
            solaris.send_message(event.player_id, "Usage: /reply <message> after receiving a message.")
            return
        end
        solaris.send_message(target_id, "[reply from " .. event.username .. "] " .. message)
        solaris.send_message(event.player_id, "[to reply target] " .. message)
    end
end

function on_plugin_storage_get_result(event: any)
    local current = pending_storage[event.request_id]
    if current == nil then return end
    pending_storage[event.request_id] = nil
    if event.key ~= current.key or event.failure ~= nil then
        solaris.send_message(current.player_id, "Location storage is unavailable.")
        return
    end
    local maximum = current.key == "warps-v1" and config.maximum_warps or config.maximum_homes_per_player
    local values = decode_positions(event.value, maximum)
    if values == nil then solaris.send_message(current.player_id, "Stored locations are invalid.") return end
    if current.kind == "home-get" or current.kind == "warp-get" then
        local target = values[current.name]
        if target == nil then solaris.send_message(current.player_id, "Location not found.") return end
        begin_teleport(current.player_id, target, current.old, current.name)
        return
    end
    local next_values = copy_positions(values)
    if current.kind == "home-set" or current.kind == "warp-set" then
        if next_values[current.name] == nil and count(next_values) >= maximum then
            solaris.send_message(current.player_id, "Location limit reached.")
            return
        end
        next_values[current.name] = current.value
    else
        if next_values[current.name] == nil then solaris.send_message(current.player_id, "Location not found.") return end
        next_values[current.name] = nil
    end
    local revision = event.version == nil and "new" or tostring(event.version)
    local request_prefix = current.key == "warps-v1" and "warps" or ("home-" .. string.sub(current.key, 7))
    local request_id = request_prefix .. "-v" .. revision
    pending_saves[request_id] = { player_id = current.player_id, key = current.key, values = next_values, name = current.name, kind = current.kind }
    solaris.storage_cas(request_id, current.key, event.version, encode_positions(next_values))
end

function on_plugin_storage_cas_result(event: any)
    local current = pending_saves[event.request_id]
    if current == nil then return end
    pending_saves[event.request_id] = nil
    if event.failure ~= nil or not event.applied then
        solaris.send_message(current.player_id, "Location changed concurrently; retry.")
        return
    end
    local verb = (current.kind == "home-delete" or current.kind == "warp-delete") and " removed." or " saved."
    solaris.send_message(current.player_id, current.name .. verb)
end

function on_player_online_result(event: any)
    local current = pending_queries[event.request_id]
    if current == nil then return end
    pending_queries[event.request_id] = nil
    if current.kind == "tpa-accept" then
        local request = current.request
        local requester: any = nil
        for _, player in ipairs(event.players) do if player.player_id == request.from_id then requester = player end end
        if requester == nil then solaris.send_message(current.player_id, "Requester is no longer online.") return end
        tpa_by_target[current.player_id] = nil
        solaris.cancel_timer("tpa-" .. tostring(current.player_id))
        begin_teleport(request.from_id, current.target_position, position(requester.x, requester.y, requester.z), "TPA")
        solaris.send_message(current.player_id, "TPA accepted.")
        return
    end
    local target = find_player(event.players, current.target_name)
    if target == nil or target.player_id == current.player_id then
        solaris.send_message(current.player_id, "Online player not found or ambiguous.")
        return
    end
    if current.kind == "tpa" then
        tpa_by_target[target.player_id] = { from_id = current.player_id, from_name = current.username }
        solaris.schedule_timer("tpa-" .. tostring(target.player_id), config.tpa_expiry_ticks)
        solaris.send_message(target.player_id, current.username .. " requested a teleport. Use /tpaccept.")
        solaris.send_message(current.player_id, "TPA request sent.")
    elseif current.kind == "msg" then
        replies[current.player_id] = target.player_id
        replies[target.player_id] = current.player_id
        solaris.send_message(target.player_id, "[from " .. current.username .. "] " .. current.message)
        solaris.send_message(current.player_id, "[to " .. target.username .. "] " .. current.message)
    end
end

function on_plugin_timer(event: any)
    local target_text = string.match(event.timer_id, "^tpa%-(%d+)$")
    local target = tonumber(target_text)
    if target ~= nil and tpa_by_target[target] ~= nil then
        tpa_by_target[target] = nil
        solaris.send_message(target, "TPA request expired.")
    end
end

function on_player_teleport_result(event: any)
    local current = pending_teleports[event.request_id]
    if current == nil then return end
    pending_teleports[event.request_id] = nil
    if event.committed then
        backs[current.player_id] = current.old
        solaris.send_message(current.player_id, current.label .. " teleport complete.")
    else
        solaris.send_message(current.player_id, "Teleport failed: " .. tostring(event.failure) .. ".")
    end
end

function on_player_died(event: any)
    backs[event.player_id] = position(event.x, event.y, event.z)
end

function on_player_left(event: any)
    backs[event.player_id] = nil
    replies[event.player_id] = nil
    tpa_by_target[event.player_id] = nil
    solaris.cancel_timer("tpa-" .. tostring(event.player_id))
    for target, request in pairs(tpa_by_target) do
        if request.from_id == event.player_id then
            tpa_by_target[target] = nil
            solaris.cancel_timer("tpa-" .. tostring(target))
        end
    end
end

function on_command_batch_rejected(_result: any)
    -- Runtime state is bounded; stale correlations expire on disconnect or TPA timer.
end
