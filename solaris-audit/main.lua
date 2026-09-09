--!strict

local config: any = solaris.config()
local storage_key = "actions-v1"
local records: { any } = {}
local queue: { any } = {}
local storage_version: any = nil
local loaded = false
local pending: any = nil
local latest_tick = 0
local sequence = 0

assert(type(config.dimension) == "string" and string.match(config.dimension, "^[a-z0-9_.-]+:[a-z0-9_./-]+$") ~= nil)
for _, key in ipairs({ "maximum_records", "maximum_pending_records", "default_lookup_count", "maximum_lookup_count", "maximum_radius" }) do
    assert(type(config[key]) == "number" and config[key] % 1 == 0)
end
assert(config.maximum_records >= 1 and config.maximum_records <= 24)
assert(config.maximum_pending_records >= 1 and config.maximum_pending_records <= 32)
assert(config.default_lookup_count >= 1 and config.default_lookup_count <= config.maximum_lookup_count)
assert(config.maximum_lookup_count <= 20)
assert(config.maximum_radius >= 1 and config.maximum_radius <= 128)

local function normalize_uuid(value: string): string?
    local without_hyphens = string.gsub(value, "-", "")
    local normalized = string.lower(without_hyphens)
    if #normalized ~= 32 or string.match(normalized, "^[0-9a-f]+$") == nil then return nil end
    return normalized
end
local function next_request(prefix: string): string sequence = sequence + 1 return prefix .. "-" .. tostring(sequence) end
local function compact(value: any): string
    local text = tostring(value or "-")
    text = string.gsub(text, "[,;|]", "_")
    return string.sub(text, 1, 48)
end
local function integer(value: number): number return math.floor(value) end

local function encode(values: { any }): string
    local rows: { string } = {}
    for _, record in ipairs(values) do
        rows[#rows + 1] = table.concat({ tostring(record.tick), record.kind, record.actor,
            tostring(record.x), tostring(record.y), tostring(record.z), compact(record.detail) }, ",")
    end
    return "v1|" .. table.concat(rows, ";")
end

local function decode(value: any): any
    local decoded: { any } = {}
    if value == nil or value == "v1|" then return decoded end
    if type(value) ~= "string" or string.sub(value, 1, 3) ~= "v1|" then return nil end
    for row in string.gmatch(string.sub(value, 4), "([^;]+)") do
        local tick_text, kind, actor_text, x_text, y_text, z_text, detail =
            string.match(row, "^(%d+),([a-z_]+),([0-9a-f]+),(-?%d+),(-?%d+),(-?%d+),([^,]*)$")
        local actor = normalize_uuid(actor_text or "")
        local tick, x, y, z = tonumber(tick_text), tonumber(x_text), tonumber(y_text), tonumber(z_text)
        if actor == nil or tick == nil or x == nil or y == nil or z == nil or kind == nil or detail == nil then return nil end
        decoded[#decoded + 1] = { tick = tick, kind = kind, actor = actor, x = x, y = y, z = z, detail = detail }
        if #decoded > config.maximum_records then return nil end
    end
    return decoded
end

local function process_queue()
    if not loaded or pending ~= nil or #queue == 0 then return end
    local record = table.remove(queue, 1)
    local next_records: { any } = {}
    for _, old in ipairs(records) do next_records[#next_records + 1] = old end
    next_records[#next_records + 1] = record
    while #next_records > config.maximum_records do table.remove(next_records, 1) end
    local revision = storage_version == nil and "new" or tostring(storage_version)
    local request_id = "append-v" .. revision
    pending = { request_id = request_id, record = record, records = next_records }
    solaris.storage_cas(request_id, storage_key, storage_version, encode(next_records))
end

local function append(event: any, kind: string, detail: any, use_block_position: boolean)
    if event.dimension ~= nil and event.dimension ~= config.dimension then return end
    local actor = normalize_uuid(event.uuid)
    if actor == nil or #queue >= config.maximum_pending_records then return end
    local x = use_block_position and event.x or integer(event.x)
    local y = use_block_position and event.y or integer(event.y)
    local z = use_block_position and event.z or integer(event.z)
    queue[#queue + 1] = { tick = latest_tick, kind = kind, actor = actor, x = x, y = y, z = z, detail = compact(detail) }
    process_queue()
end

function on_server_started(_event: any)
    solaris.storage_get("audit-load", storage_key)
end
function on_server_tick(event: any) latest_tick = math.max(latest_tick, event.tick) end
function on_player_block_broken(event: any) append(event, "break", event.block_id, true) end
function on_player_block_placed(event: any) append(event, "place", event.block_id, true) end
function on_player_item_crafted(event: any) append(event, "craft", event.item_id .. ":" .. tostring(event.count), false) end
function on_player_item_picked_up(event: any) append(event, "pickup", event.item_id .. ":" .. tostring(event.count), false) end
function on_player_entity_killed(event: any) append(event, "kill", event.entity_type, false) end
function on_player_entity_interacted(event: any) append(event, "interact", event.entity_type, false) end
function on_player_died(event: any) append(event, "death", "player", false) end

function on_player_command(event: any)
    if event.root ~= "audit" then return end
    if not event.operator then solaris.send_message(event.player_id, "Only an operator can query audit history.") return end
    if not loaded then solaris.send_message(event.player_id, "Audit history is still loading.") return end
    local words: { string } = {}
    for word in string.gmatch(event.arguments, "%S+") do words[#words + 1] = word end
    if words[1] == "rollback" then
        solaris.send_message(event.player_id, "Rollback unavailable: API 0.6 events omit exact prior block state and block-entity data.")
        return
    end
    local mode = "all"
    local actor: string? = nil
    local radius = 0
    local wanted = config.default_lookup_count
    if words[1] == "actor" and words[2] ~= nil then
        mode = "actor"
        actor = normalize_uuid(words[2])
        wanted = tonumber(words[3]) or config.default_lookup_count
        if actor == nil or #words > 3 then solaris.send_message(event.player_id, "Usage: /audit actor <uuid> [count]") return end
    elseif words[1] == "here" then
        mode = "here"
        radius = tonumber(words[2]) or 8
        wanted = tonumber(words[3]) or config.default_lookup_count
        if #words > 3 or radius % 1 ~= 0 or radius < 0 or radius > config.maximum_radius then solaris.send_message(event.player_id, "Usage: /audit here <radius> [count]") return end
    elseif words[1] == "since" then
        mode = "since"
        radius = tonumber(words[2]) or -1
        wanted = tonumber(words[3]) or config.default_lookup_count
        if #words > 3 or radius % 1 ~= 0 or radius < 0 or radius > 630720000 then solaris.send_message(event.player_id, "Usage: /audit since <ticks> [count]") return end
    elseif words[1] ~= nil then
        wanted = tonumber(words[1]) or -1
        if #words > 1 then wanted = -1 end
    end
    if wanted % 1 ~= 0 or wanted < 1 or wanted > config.maximum_lookup_count then
        solaris.send_message(event.player_id, "Count must be 1.." .. tostring(config.maximum_lookup_count) .. ".") return
    end
    local sent = 0
    for index = #records, 1, -1 do
        local record = records[index]
        local matches = mode == "all" or (mode == "actor" and record.actor == actor)
            or (mode == "here" and math.abs(record.x - event.x) <= radius and math.abs(record.z - event.z) <= radius)
            or (mode == "since" and record.tick >= math.max(0, latest_tick - radius))
        if matches then
            solaris.send_message(event.player_id, "t" .. tostring(record.tick) .. " " .. record.kind .. " " .. record.actor .. " @ "
                .. tostring(record.x) .. "," .. tostring(record.y) .. "," .. tostring(record.z) .. " " .. record.detail)
            sent = sent + 1
            if sent >= wanted then break end
        end
    end
    if sent == 0 then solaris.send_message(event.player_id, "No matching bounded audit records.") end
end

function on_plugin_storage_get_result(event: any)
    if event.key ~= storage_key or event.failure ~= nil then loaded = false return end
    local decoded = decode(event.value)
    if decoded == nil then loaded = false return end
    records = decoded
    storage_version = event.version
    loaded = true
    process_queue()
end

function on_plugin_storage_cas_result(event: any)
    local current = pending
    if current == nil or current.request_id ~= event.request_id then return end
    pending = nil
    if event.failure ~= nil or not event.applied then
        table.insert(queue, 1, current.record)
        loaded = false
        solaris.storage_get(next_request("reload"), storage_key)
        return
    end
    records = current.records
    storage_version = event.version
    process_queue()
end

function on_command_batch_rejected(_result: any)
    if pending ~= nil then
        table.insert(queue, 1, pending.record)
        pending = nil
    end
end
