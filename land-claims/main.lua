--!strict

local config: any = solaris.config()
local storage_key = "claims:v1"
local claims: any = {}
local storage_version: any = nil
local loaded = false
local pending_save: any = nil
local pending_zone_load: any = nil
local request_sequence = 0
local last_batch: any = nil

assert(type(config.dimension) == "string")
assert(string.match(config.dimension, "^[a-z0-9_.-]+:[a-z0-9_./-]+$") ~= nil)
assert(type(config.minimum_y) == "number" and config.minimum_y % 1 == 0)
assert(type(config.maximum_y) == "number" and config.maximum_y % 1 == 0)
assert(config.minimum_y <= config.maximum_y)
assert(type(config.maximum_claims) == "number" and config.maximum_claims % 1 == 0)
assert(config.maximum_claims >= 1 and config.maximum_claims <= 48)

local function next_request(prefix: string): string
    request_sequence = request_sequence + 1
    return prefix .. "-" .. tostring(request_sequence)
end

local function normalize_uuid(uuid: string): string?
    local without_hyphens = string.gsub(uuid, "-", "")
    local normalized = string.lower(without_hyphens)
    if #normalized ~= 32 or string.match(normalized, "^[0-9a-f]+$") == nil then
        return nil
    end
    return normalized
end

local function chunk_coordinate(value: number): number
    return math.floor(value / 16)
end

local function coordinate_id(value: number): string
    return value < 0 and ("n" .. tostring(-value)) or ("p" .. tostring(value))
end

local function claim_key(chunk_x: number, chunk_z: number): string
    return tostring(chunk_x) .. "," .. tostring(chunk_z)
end

local function zone_id(claim: any): string
    return "claim-" .. claim.owner .. "-" .. coordinate_id(claim.x) .. "-" .. coordinate_id(claim.z)
end

local function register_claim(claim: any)
    solaris.upsert_protected_zone(
        zone_id(claim),
        config.dimension,
        claim.owner,
        claim.x * 16,
        config.minimum_y,
        claim.z * 16,
        claim.x * 16 + 15,
        config.maximum_y,
        claim.z * 16 + 15
    )
end

local function copy_claims(source: any): any
    local copied: any = {}
    for key, claim in pairs(source) do
        copied[key] = { x = claim.x, z = claim.z, owner = claim.owner }
    end
    return copied
end

local function encode_claims(values: any): string
    local keys: { string } = {}
    for key in pairs(values) do
        keys[#keys + 1] = key
    end
    table.sort(keys)
    local encoded: { string } = {}
    for _, key in ipairs(keys) do
        local claim = values[key]
        encoded[#encoded + 1] = tostring(claim.x) .. "," .. tostring(claim.z) .. "," .. claim.owner
    end
    return "v1|" .. table.concat(encoded, ";")
end

local function decode_claims(value: string?): any
    local decoded: any = {}
    if value == nil or value == "v1|" then
        return decoded
    end
    if string.sub(value, 1, 3) ~= "v1|" then
        return nil
    end
    local count = 0
    for row in string.gmatch(string.sub(value, 4), "([^;]+)") do
        local x_text, z_text, owner_text = string.match(row, "^(-?%d+),(-?%d+),([0-9a-f]+)$")
        local x = tonumber(x_text)
        local z = tonumber(z_text)
        local owner = normalize_uuid(owner_text or "")
        if x == nil or z == nil or owner == nil then
            return nil
        end
        count = count + 1
        if count > config.maximum_claims then
            return nil
        end
        local key = claim_key(x, z)
        if decoded[key] ~= nil then
            return nil
        end
        decoded[key] = { x = x, z = z, owner = owner }
    end
    return decoded
end

local function save_claims(player_id, next_claims, action, claim)
    if pending_save ~= nil then
        solaris.send_message(player_id, "Another claim update is still committing.")
        return
    end
    local request_id = next_request("save")
    pending_save = {
        request_id = request_id,
        player_id = player_id,
        previous_claims = claims,
        claims = next_claims,
        action = action,
        claim = claim,
        stage = "save",
    }
    last_batch = { kind = "save", request_id = request_id }
    solaris.storage_cas(request_id, storage_key, storage_version, encode_claims(next_claims))
end

function on_server_started(_event)
    local request_id = next_request("load")
    last_batch = { kind = "load", request_id = request_id }
    solaris.storage_get(request_id, storage_key)
end

function on_player_command(event)
    last_batch = nil
    if event.root ~= "claim" then
        return
    end
    if not loaded then
        solaris.send_message(event.player_id, "Claims are still loading.")
        return
    end
    local action = string.match(event.arguments, "^%s*(%S*)%s*$") or ""
    local x = chunk_coordinate(event.x)
    local z = chunk_coordinate(event.z)
    local key = claim_key(x, z)
    local owner = normalize_uuid(event.uuid)
    local current = claims[key]
    if action == "" or action == "status" then
        if current == nil then
            solaris.send_message(event.player_id, "This chunk is unclaimed.")
        elseif current.owner == owner then
            solaris.send_message(event.player_id, "You own this chunk claim.")
        else
            solaris.send_message(event.player_id, "This chunk is claimed by another player.")
        end
        return
    end
    if action == "create" then
        if current ~= nil then
            solaris.send_message(event.player_id, "This chunk is already claimed.")
            return
        end
        local count = 0
        for _ in pairs(claims) do count = count + 1 end
        if count >= config.maximum_claims then
            solaris.send_message(event.player_id, "The server claim limit is reached.")
            return
        end
        local next_claims = copy_claims(claims)
        local claim = { x = x, z = z, owner = owner }
        next_claims[key] = claim
        save_claims(event.player_id, next_claims, "create", claim)
    elseif action == "remove" then
        if current == nil then
            solaris.send_message(event.player_id, "This chunk is not claimed.")
            return
        end
        if current.owner ~= owner and not event.operator then
            solaris.send_message(event.player_id, "Only the owner or an operator can remove this claim.")
            return
        end
        local next_claims = copy_claims(claims)
        next_claims[key] = nil
        save_claims(event.player_id, next_claims, "remove", current)
    else
        solaris.send_message(event.player_id, "Usage: /claim [status|create|remove]")
    end
end

function on_plugin_storage_get_result(event)
    last_batch = nil
    if event.key ~= storage_key then
        return
    end
    if event.failure ~= nil then
        loaded = false
        return
    end
    local decoded = decode_claims(event.value)
    if decoded == nil then
        loaded = false
        return
    end
    local previous_claims = claims
    local remaining = {}
    local command_count = 0
    last_batch = { kind = "zone_load" }
    for key, claim in pairs(previous_claims) do
        if decoded[key] == nil then
            remaining[zone_id(claim)] = true
            command_count = command_count + 1
            solaris.remove_zone(zone_id(claim))
        end
    end
    claims = decoded
    storage_version = event.version
    for _, claim in pairs(claims) do
        remaining[zone_id(claim)] = true
        command_count = command_count + 1
        register_claim(claim)
    end
    if command_count == 0 then
        loaded = true
        pending_zone_load = nil
        last_batch = nil
    else
        loaded = false
        pending_zone_load = { remaining = remaining, failed = false }
    end
end

function on_plugin_storage_cas_result(event)
    last_batch = nil
    local pending = pending_save
    if pending == nil or pending.request_id ~= event.request_id then
        return
    end
    if pending.stage == "rollback" then
        pending_save = nil
        if event.failure ~= nil or not event.applied then
            loaded = false
            solaris.send_message(pending.player_id, "Claim protection failed and storage needs an operator check.")
            return
        end
        claims = pending.previous_claims
        storage_version = event.version
        solaris.send_message(pending.player_id, "Claim protection was unavailable; no claim change was kept.")
        return
    end
    if pending.stage ~= "save" then
        return
    end
    if event.failure ~= nil or not event.applied then
        pending_save = nil
        solaris.send_message(pending.player_id, "Claims changed concurrently; retry.")
        local request_id = next_request("reload")
        last_batch = { kind = "reload", request_id = request_id }
        solaris.storage_get(request_id, storage_key)
        return
    end
    claims = pending.claims
    storage_version = event.version
    pending.stage = "zone"
    last_batch = { kind = "transition_zone" }
    if pending.action == "create" then
        register_claim(pending.claim)
    else
        solaris.remove_zone(zone_id(pending.claim))
    end
end

local function rollback_pending_change()
    local pending = pending_save
    if pending == nil then
        return
    end
    local request_id = next_request("rollback")
    pending.request_id = request_id
    pending.stage = "rollback"
    last_batch = { kind = "rollback", request_id = request_id }
    solaris.storage_cas(
        request_id,
        storage_key,
        storage_version,
        encode_claims(pending.previous_claims)
    )
end

function on_zone_command_result(event)
    last_batch = nil
    if pending_zone_load ~= nil and pending_zone_load.remaining[event.zone_id] then
        pending_zone_load.remaining[event.zone_id] = nil
        if not event.accepted then
            pending_zone_load.failed = true
        end
        if next(pending_zone_load.remaining) == nil then
            loaded = not pending_zone_load.failed
            pending_zone_load = nil
        end
        return
    end
    local pending = pending_save
    if pending == nil or pending.stage ~= "zone" or zone_id(pending.claim) ~= event.zone_id then
        return
    end
    if not event.accepted then
        rollback_pending_change()
        return
    end
    pending_save = nil
    if pending.action == "create" then
        solaris.send_message(pending.player_id, "Chunk claimed. Breaking and placing are now protected.")
    else
        solaris.send_message(pending.player_id, "Chunk claim removed.")
    end
end

function on_command_batch_rejected(_result)
    local batch = last_batch
    last_batch = nil
    if batch == nil then
        return
    end
    if batch.kind == "save" and pending_save ~= nil
        and pending_save.request_id == batch.request_id then
        pending_save = nil
    elseif batch.kind == "transition_zone" then
        rollback_pending_change()
    elseif batch.kind == "rollback" then
        pending_save = nil
        loaded = false
    elseif batch.kind == "zone_load" then
        pending_zone_load = nil
        loaded = false
    elseif batch.kind == "load" or batch.kind == "reload" then
        loaded = false
    end
end
