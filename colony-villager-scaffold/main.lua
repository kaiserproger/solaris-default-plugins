--!strict

-- Colony identity, roles, orders, home policy, and durable intent live in Luau.
-- Rust exposes only bounded storage, zones, an opaque villager binding, and
-- generic idle/follow-position goals through the regional entity owner.

local raw_config: any = solaris.config()
local colony_config: any = raw_config.colony
local villager_config: any = raw_config.villagers
local limits_config: any = raw_config.limits
if colony_config == nil
    or colony_config.home == nil
    or colony_config.zone == nil
    or villager_config == nil
    or limits_config == nil
then
    error("colony-villager-scaffold requires config.toml")
end

type Vec3Config = { x: number, y: number, z: number }
type ColonyConfig = {
    colony: { id: string, name: string, dimension: string, home: Vec3Config },
    zone: { id: string, minimum: Vec3Config, maximum: Vec3Config },
    binding_radius: number,
    home_speed: number,
    default_role: string,
    default_order: string,
    roles: { string },
    orders: { string },
    max_pending_requests: number,
    max_active_players: number,
    max_active_members: number,
    max_generation: number,
}
local config: ColonyConfig = {
    colony = {
        id = colony_config.id,
        name = colony_config.name,
        dimension = colony_config.dimension,
        home = {
            x = colony_config.home.x,
            y = colony_config.home.y,
            z = colony_config.home.z,
        },
    },
    zone = {
        id = colony_config.zone.id,
        minimum = {
            x = colony_config.zone.min_x,
            y = colony_config.zone.min_y,
            z = colony_config.zone.min_z,
        },
        maximum = {
            x = colony_config.zone.max_x,
            y = colony_config.zone.max_y,
            z = colony_config.zone.max_z,
        },
    },
    binding_radius = villager_config.binding_radius,
    home_speed = villager_config.home_speed,
    default_role = villager_config.default_role,
    default_order = villager_config.default_order,
    roles = villager_config.roles,
    orders = villager_config.orders,
    max_pending_requests = limits_config.max_pending_requests,
    max_active_players = limits_config.max_active_players,
    max_active_members = limits_config.max_active_members,
    max_generation = 999999,
}

local function valid_text(value: any, maximum: number): boolean
    return type(value) == "string"
        and #value > 0
        and #value <= maximum
        and string.find(value, "|", 1, true) == nil
end

local function valid_number(value: any): boolean
    return type(value) == "number" and value == value and math.abs(value) < math.huge
end

if not valid_text(config.colony.id, 128)
    or not valid_text(config.colony.name, 128)
    or not valid_text(config.colony.dimension, 256)
    or not valid_text(config.zone.id, 128)
    or not valid_number(config.colony.home.x)
    or not valid_number(config.colony.home.y)
    or not valid_number(config.colony.home.z)
    or not valid_number(config.binding_radius)
    or config.binding_radius <= 0
    or config.binding_radius > 64
    or not valid_number(config.home_speed)
    or config.home_speed <= 0
    or config.home_speed > 4
    or type(config.roles) ~= "table"
    or type(config.orders) ~= "table"
    or type(config.max_pending_requests) ~= "number"
    or config.max_pending_requests < 1
    or config.max_pending_requests > 128
    or type(config.max_active_players) ~= "number"
    or config.max_active_players < 1
    or config.max_active_players > 256
    or type(config.max_active_members) ~= "number"
    or config.max_active_members < 1
    or config.max_active_members > 8
then
    error("invalid colony-villager-scaffold config")
end

local role_allowed: { [string]: boolean } = {}
local order_allowed: { [string]: boolean } = {}
for _, role in ipairs(config.roles) do
    if not valid_text(role, 64) then
        error("invalid configured colony role")
    end
    role_allowed[role] = true
end
for _, order in ipairs(config.orders) do
    if order ~= "home" and order ~= "hold" and order ~= "follow" then
        error("configured orders must be home, hold, or follow")
    end
    order_allowed[order] = true
end
if not role_allowed[config.default_role] or not order_allowed[config.default_order] then
    error("default colony role/order must be declared")
end

local function role_default_order(role: string): string
    local order = "home"
    if role == "guard" then
        order = "follow"
    end
    if not order_allowed[order] then
        return config.default_order
    end
    return order
end

type MemberStatus = "recruiting" | "active" | "rejected" | "released"
type ColonyRecord = {
    status: MemberStatus,
    role: string,
    order: string,
    generation: number,
}
type MemberEntry = {
    uuid: string?,
    key: string,
    version: number?,
    value: ColonyRecord,
}
type StatusCollect = {
    remaining: number,
    lines: { [number]: string },
}
type ActiveBinding = {
    lease_id: string,
    expires_at_tick: number,
}
type BatchInfo = {
    kind: string,
    request_id: string?,
    player_id: number?,
}
type AfterAction = {
    kind: string,
    slot: number?,
    x: number?,
    y: number?,
    z: number?,
    failure: string?,
    field: string?,
}

type SlotMembers = { [number]: MemberEntry }

type PendingGet = {
    player_id: number?,
    uuid: string?,
    action: string,
    argument: string?,
    key: string,
    slot: number?,
    x: number?,
    y: number?,
    z: number?,
}
type PendingCas = {
    player_id: number?,
    uuid: string?,
    key: string,
    next_record: ColonyRecord?,
    after: AfterAction,
    slot: number?,
}
type PendingLease = {
    player_id: number,
    uuid: string?,
    key: string,
    version: number?,
    slot: number,
    record: ColonyRecord,
    purpose: string,
    retry_binding: boolean,
    binding_expires_at_tick: number?,
    lease_id: string?,
    x: number?,
    y: number?,
    z: number?,
}
local startup_state: string = "starting"
local pending_gets: { [string]: PendingGet } = {}
local pending_get_by_player: { [number]: { [string]: boolean } } = {}
local pending_cas: { [string]: PendingCas } = {}
local pending_bindings: { [string]: PendingLease } = {}
local pending_releases: { [string]: PendingLease } = {}
local pending_goals: { [string]: PendingLease } = {}
local status_collects: { [number]: StatusCollect } = {}
local active_bindings: { [number]: { [number]: ActiveBinding } } = {}
local records: { [number]: SlotMembers } = {}
local zone_seen: { [number]: string } = {}
local deferred_notices: { [number]: string } = {}
local last_batch: BatchInfo? = nil

local function table_size(values: { [any]: any }): number
    local count = 0
    for _ in pairs(values) do
        count = count + 1
    end
    return count
end

local function pending_count(): number
    return table_size(pending_gets)
        + table_size(pending_cas)
        + table_size(pending_bindings)
        + table_size(pending_goals)
        + table_size(pending_releases)
end
local function member_key(uuid: string, slot: number): string
    if slot == 1 then
        return "member:" .. uuid
    end
    return "member:" .. uuid .. ":" .. tostring(slot)
end

local function active_slots(player_id: number): { number }
    local found: { number } = {}
    local members = records[player_id]
    if members ~= nil then
        for slot = 1, config.max_active_members do
            local entry = members[slot]
            if entry ~= nil and entry.value ~= nil and entry.value.status == "active" then
                found[#found + 1] = slot
            end
        end
    end
    return found
end

local function next_free_slot(player_id: number): number?
    local members = records[player_id]
    for slot = 1, config.max_active_members do
        local entry = members ~= nil and members[slot] or nil
        if entry == nil or entry.value == nil
            or (entry.value.status ~= "active" and entry.value.status ~= "recruiting")
        then
            return slot
        end
    end
    return nil
end

local function metadata_key(): string
    return "colony:" .. config.colony.id
end

local function metadata_value(): string
    local home = config.colony.home
    return table.concat({
        "v1",
        config.colony.id,
        config.colony.name,
        config.colony.dimension,
        tostring(home.x),
        tostring(home.y),
        tostring(home.z),
    }, "|")
end

local function encode_record(record: ColonyRecord): string
    return table.concat({
        "v2",
        record.status,
        record.role,
        record.order,
        tostring(record.generation),
    }, "|")
end

local function decode_record(value: any): (ColonyRecord?, string?)
    if value == nil then
        return nil, nil
    end
    local status, role, order, generation = string.match(
        value,
        "^v2|([a-z_]+)|([a-z_]+)|([a-z_]+)|(%d+)$"
    )
    local generation_number = tonumber(generation)
    if status == nil or role == nil or order == nil or generation == nil then
        return nil, "invalid"
    end
    if (status ~= "recruiting" and status ~= "active" and status ~= "rejected" and status ~= "released")
        or not role_allowed[role]
        or not order_allowed[order]
        or generation_number == nil
        or generation_number > config.max_generation
    then
        return nil, "invalid"
    end
    return {
        status = status,
        role = role,
        order = order,
        generation = generation_number,
    }, nil
end

local function copy_record(record: ColonyRecord): ColonyRecord
    return {
        status = record.status,
        role = record.role,
        order = record.order,
        generation = record.generation,
    }
end

local function remember_notice(player_id: number?, message: string)
    if player_id ~= nil then
        deferred_notices[player_id] = message
    end
end

local function send_message(player_id: number?, message: string)
    if last_batch == nil then
        last_batch = { kind = "message", player_id = player_id }
    end
    solaris.send_message(player_id, message)
end

local function send_notice(player_id: number)
    local notice = deferred_notices[player_id]
    if notice ~= nil then
        deferred_notices[player_id] = nil
        send_message(player_id, notice)
    end
end

local function request_id(prefix: string, player_id: number?, version: number?, slot: number?): string
    return prefix
        .. "-" .. tostring(player_id or 0)
        .. "-" .. (version == nil and "new" or tostring(version))
        .. "-" .. tostring(slot or 0)
end

local function queue_get(
    player_id: number?,
    uuid: string?,
    action: string,
    argument: string?,
    id: string,
    key: string,
    slot: number?
): boolean
    if pending_count() >= config.max_pending_requests then
        remember_notice(player_id, "Colony request rejected: pending-request limit reached.")
        return false
    end
    pending_gets[id] = {
        player_id = player_id,
        uuid = uuid,
        action = action,
        argument = argument,
        key = key,
        slot = slot,
    }
    if player_id ~= nil then
        local ids = pending_get_by_player[player_id]
        if ids == nil then
            ids = {}
            pending_get_by_player[player_id] = ids
        end
        ids[id] = true
    end
    last_batch = { kind = "get", request_id = id, player_id = player_id }
    solaris.storage_get(id, key)
    return true
end

local function queue_cas(
    player_id: number?,
    uuid: string?,
    key: string,
    expected_version: number?,
    value: string,
    next_record: ColonyRecord?,
    after: AfterAction,
    prefix: string
): boolean
    if pending_count() >= config.max_pending_requests then
        remember_notice(player_id, "Colony update rejected: pending-request limit reached.")
        return false
    end
    local id = request_id(prefix, player_id, expected_version, after.slot)
    pending_cas[id] = {
        player_id = player_id,
        uuid = uuid,
        key = key,
        next_record = next_record,
        after = after,
        slot = after.slot,
    }
    last_batch = { kind = "cas", request_id = id, player_id = player_id }
    solaris.storage_cas(id, key, expected_version, value)
    return true
end

local function queue_binding(pending: PendingLease): boolean
    if pending_count() >= config.max_pending_requests then
        remember_notice(pending.player_id, "Villager binding rejected: pending-request limit reached.")
        return false
    end
    local id = request_id("bind", pending.player_id, pending.version, pending.slot)
    pending_bindings[id] = pending
    last_batch = { kind = "binding", request_id = id, player_id = pending.player_id }
    solaris.bind_nearest_villager(id, pending.x, pending.y, pending.z, config.binding_radius)
    return true
end

local function expected_goal(order: string): string
    if order == "home" or order == "follow" then
        return "follow_position"
    end
    return "idle"
end

local function queue_goal(pending: PendingLease, lease_id: string): boolean
    if pending_count() >= config.max_pending_requests then
        remember_notice(pending.player_id, "Villager goal rejected: pending-request limit reached.")
        return false
    end
    local id = request_id("goal", pending.player_id, pending.version, pending.slot)
    pending.lease_id = lease_id
    pending_goals[id] = pending
    last_batch = { kind = "goal", request_id = id, player_id = pending.player_id }
    if pending.purpose == "dismiss" then
        solaris.set_villager_idle(id, lease_id)
    elseif pending.record.order == "home" then
        local home = config.colony.home
        solaris.move_villager_to(id, lease_id, home.x, home.y, home.z, config.home_speed)
    elseif pending.record.order == "follow" then
        solaris.move_villager_to(id, lease_id, pending.x, pending.y, pending.z, config.home_speed)
    else
        solaris.set_villager_idle(id, lease_id)
    end
    return true
end

local function queue_release(pending: PendingLease): boolean
    if pending_count() >= config.max_pending_requests then
        remember_notice(pending.player_id, "Villager release rejected: pending-request limit reached.")
        return false
    end
    local id = request_id("release", pending.player_id, pending.version, pending.slot)
    pending_releases[id] = pending
    last_batch = { kind = "release", request_id = id, player_id = pending.player_id }
    solaris.release_villager_binding(id, pending.lease_id)
    return true
end

local function split_arguments(arguments: string): { string }?
    local values = {}
    for value in string.gmatch(arguments, "%S+") do
        if #values == 3 then
            return nil
        end
        values[#values + 1] = value
    end
    return values
end

local function player_busy(player_id: number): boolean
    for _, pending in pairs(pending_cas) do
        if pending.player_id == player_id then
            return true
        end
    end
    for _, pending in pairs(pending_bindings) do
        if pending.player_id == player_id then
            return true
        end
    end
    for _, pending in pairs(pending_goals) do
        if pending.player_id == player_id then
            return true
        end
    end
    for _, pending in pairs(pending_releases) do
        if pending.player_id == player_id then
            return true
        end
    end
    return false
end

local function refresh_follow_goal(player_id: number, uuid: string, slot: number, x: number, y: number, z: number)
    local bindings = active_bindings[player_id]
    local binding = bindings ~= nil and bindings[slot] or nil
    if binding == nil then
        return
    end
    local members = records[player_id]
    local entry = members ~= nil and members[slot] or nil
    if entry == nil then
        return
    end
    if pending_count() >= config.max_pending_requests then
        return
    end
    local id = request_id("goal", player_id, entry.version, slot)
    pending_goals[id] = {
        player_id = player_id,
        uuid = uuid,
        slot = slot,
        key = entry.key,
        version = entry.version,
        record = entry.value,
        purpose = "refresh_follow",
        binding_expires_at_tick = binding.expires_at_tick,
        retry_binding = true,
        x = x,
        y = y,
        z = z,
    }
    last_batch = { kind = "goal", request_id = id, player_id = player_id }
    solaris.move_villager_to(id, binding.lease_id, x, y, z, config.home_speed)
end

local function refresh_follow_for_position_event(player_id: number, uuid: string, x: number, y: number, z: number)
    if player_busy(player_id) then
        return
    end
    local members = records[player_id]
    if members == nil then
        return
    end
    for slot = 1, config.max_active_members do
        local entry = members[slot]
        if entry ~= nil and entry.value ~= nil
            and entry.value.status == "active"
            and entry.value.order == "follow"
        then
            local bindings = active_bindings[player_id]
            local binding = bindings ~= nil and bindings[slot] or nil
            if binding ~= nil then
                refresh_follow_goal(player_id, uuid, slot, x, y, z)
            end
        end
    end
end

local function status_message(record: ColonyRecord?, slot: number): string
    if startup_state ~= "ready" then
        return "Colony unavailable: state=" .. startup_state .. "."
    end
    if record == nil then
        return config.colony.name .. ": no villager is recruited for this player."
    end
    return config.colony.name
        .. " member " .. tostring(slot)
        .. ": status=" .. record.status
        .. ", role=" .. record.role
        .. ", order=" .. record.order
        .. ", generation=" .. tostring(record.generation) .. "."
end

local function queue_goal_for_record(pending: PendingLease, lease_id: string)
    pending.binding_expires_at_tick = pending.binding_expires_at_tick or 0
    queue_goal(pending, lease_id)
end
local function collect_status_slot(player_id: number?, record: ColonyRecord?, slot: number)
    if player_id == nil then
        return
    end
    local collect = status_collects[player_id]
    if collect == nil then
        return
    end
    if record ~= nil and record.status == "active" then
        collect.lines[slot] = status_message(record, slot)
    end
    collect.remaining = collect.remaining - 1
    if collect.remaining <= 0 then
        status_collects[player_id] = nil
        local sent = false
        for i = 1, config.max_active_members do
            if collect.lines[i] ~= nil then
                send_message(player_id, collect.lines[i])
                sent = true
            end
        end
        if not sent then
            send_message(player_id, config.colony.name .. ": no villager is recruited for this player.")
        end
    end
end


local function handle_player_state(pending: PendingGet, value: any, version: any)
    local player_id = pending.player_id
    local slot = pending.slot
    if player_id == nil or slot == nil then
        return
    end
    local record, decode_error = decode_record(value)
    if decode_error ~= nil then
        send_message(player_id, "Colony state rejected: invalid durable record.")
        return
    end
    local members = records[player_id]
    if members == nil then
        members = {}
        records[player_id] = members
    end
    if record == nil then
        members[slot] = nil
    else
        members[slot] = {
            uuid = pending.uuid,
            key = pending.key,
            version = version,
            value = record,
        }
    end

    if pending.action == "status_collect" then
        collect_status_slot(player_id, record, slot)
        return
    end
    if startup_state ~= "ready" then
        send_message(player_id, status_message(record, slot))
        return
    end

    if pending.action == "recruit" then
        if record ~= nil and (record.status == "active" or record.status == "recruiting") then
            send_message(player_id, "Recruitment ignored: member " .. tostring(slot) .. " is already active.")
            return
        end
        local role = pending.argument or config.default_role
        if not role_allowed[role] then
            send_message(player_id, "Recruitment rejected: unsupported role.")
            return
        end
        local recruit_generation = 1
        if record ~= nil then
            recruit_generation = record.generation + 1
        end
        local next_record: ColonyRecord = {
            status = "recruiting",
            role = role,
            order = role_default_order(role),
            generation = recruit_generation,
        }
        queue_cas(
            player_id,
            pending.uuid,
            pending.key,
            version,
            encode_record(next_record),
            next_record,
            { kind = "bind", slot = slot, x = pending.x, y = pending.y, z = pending.z },
            "recruit"
        )
        return
    end

    if record == nil or record.status ~= "active" then
        send_message(player_id, "Update rejected: member " .. tostring(slot) .. " is not active.")
        return
    end
    local next_record = copy_record(record)
    next_record.generation = next_record.generation + 1
    if next_record.generation > config.max_generation then
        send_message(player_id, "Update rejected: generation limit reached.")
        return
    end
    if pending.action == "dismiss" then
        local bindings = active_bindings[player_id]
        local binding = bindings ~= nil and bindings[slot] or nil
        if binding == nil then
            next_record.status = "released"
            queue_cas(
                player_id,
                pending.uuid,
                pending.key,
                version,
                encode_record(next_record),
                next_record,
                { kind = "dismissed", slot = slot },
                "dismiss"
            )
        else
            local dismiss_record = copy_record(record)
            dismiss_record.order = "hold"
            queue_goal_for_record({
                player_id = player_id,
                uuid = pending.uuid,
                key = pending.key,
                version = version,
                record = dismiss_record,
                purpose = "dismiss",
                binding_expires_at_tick = binding.expires_at_tick,
                retry_binding = false,
                slot = slot,
            }, binding.lease_id)
        end
        return
    end
    if pending.action == "role" and pending.argument ~= nil then
        next_record.role = pending.argument
        next_record.order = role_default_order(pending.argument)
    elseif pending.action == "order" and pending.argument ~= nil then
        next_record.order = pending.argument
    else
        send_message(player_id, "Colony command rejected: unsupported action.")
        return
    end
    queue_cas(
        player_id,
        pending.uuid,
        pending.key,
        version,
        encode_record(next_record),
        next_record,
        (pending.action == "order" or pending.action == "role")
            and { kind = "apply_order", slot = slot, x = pending.x, y = pending.y, z = pending.z }
            or { kind = "updated", field = pending.action },
        pending.action
    )
end

local function clear_cached_slot(player_id: number, slot: number)
    local members = records[player_id]
    if members ~= nil then
        members[slot] = nil
    end
end

local function clear_slot(player_id: number, slot: number)
    for id, pending in pairs(pending_cas) do
        if pending.player_id == player_id and pending.slot == slot then
            pending_cas[id] = nil
        end
    end
    for id, pending in pairs(pending_bindings) do
        if pending.player_id == player_id and pending.slot == slot then
            pending_bindings[id] = nil
        end
    end
    for id, pending in pairs(pending_goals) do
        if pending.player_id == player_id and pending.slot == slot then
            pending_goals[id] = nil
        end
    end
    for id, pending in pairs(pending_releases) do
        if pending.player_id == player_id and pending.slot == slot then
            pending_releases[id] = nil
        end
    end
    local bindings = active_bindings[player_id]
    if bindings ~= nil then
        bindings[slot] = nil
    end
    local members = records[player_id]
    if members ~= nil then
        members[slot] = nil
    end
end

local function clear_player(player_id: number)
    local get_ids = pending_get_by_player[player_id]
    if get_ids ~= nil then
        for id, _ in pairs(get_ids) do
            pending_gets[id] = nil
        end
        pending_get_by_player[player_id] = nil
    end
    for id, pending in pairs(pending_cas) do
        if pending.player_id == player_id then
            pending_cas[id] = nil
        end
    end
    for id, pending in pairs(pending_bindings) do
        if pending.player_id == player_id then
            pending_bindings[id] = nil
        end
    end
    for id, pending in pairs(pending_goals) do
        if pending.player_id == player_id then
            pending_goals[id] = nil
        end
    end
    for id, pending in pairs(pending_releases) do
        if pending.player_id == player_id then
            pending_releases[id] = nil
        end
    end
    active_bindings[player_id] = nil
    records[player_id] = nil
    status_collects[player_id] = nil
    zone_seen[player_id] = nil
    deferred_notices[player_id] = nil
end

function on_server_started(_event: any)
    startup_state = "metadata_pending"
    last_batch = { kind = "startup" }
    solaris.upsert_zone(
        config.zone.id,
        config.colony.dimension,
        config.zone.minimum.x,
        config.zone.minimum.y,
        config.zone.minimum.z,
        config.zone.maximum.x,
        config.zone.maximum.y,
        config.zone.maximum.z
    )
    queue_get(nil, nil, "metadata", nil, "load-colony-metadata", metadata_key())
end

function on_player_joined(event: any)
    if startup_state == "ready" then
        send_message(event.player_id, config.colony.name .. " plugin ready.")
    end
end

function on_player_zone_entered(event: any)
    last_batch = nil
    if event.zone_id ~= config.zone.id or zone_seen[event.player_id] == event.uuid then
        return
    end
    zone_seen[event.player_id] = event.uuid
    send_notice(event.player_id)
    send_message(event.player_id, config.colony.name .. ": use /colony status, /colony recruit [role], or right-click a villager to recruit it. /colony order follow makes it follow you.")
end

function on_player_command(event: any)
    last_batch = nil
    if event.root ~= "colony" then
        return
    end
    send_notice(event.player_id)
    local player_gets = pending_get_by_player[event.player_id]
    if player_gets ~= nil and next(player_gets) ~= nil then
        send_message(event.player_id, "Colony request rejected: another request is pending.")
        return
    end
    for _, pending in pairs(pending_cas) do
        if pending.player_id == event.player_id then
            send_message(event.player_id, "Colony request rejected: another request is pending.")
            return
        end
    end
    for _, pending in pairs(pending_bindings) do
        if pending.player_id == event.player_id then
            send_message(event.player_id, "Colony request rejected: another request is pending.")
            return
        end
    end
    for _, pending in pairs(pending_goals) do
        if pending.player_id == event.player_id then
            send_message(event.player_id, "Colony request rejected: another request is pending.")
            return
        end
    end
    for _, pending in pairs(pending_releases) do
        if pending.player_id == event.player_id then
            send_message(event.player_id, "Colony request rejected: another request is pending.")
            return
        end
    end
    if records[event.player_id] == nil
        and table_size(records) + table_size(pending_gets) >= config.max_active_players
    then
        send_message(event.player_id, "Colony request rejected: active-player limit reached.")
        return
    end

    local arguments = split_arguments(event.arguments)
    if arguments == nil or #arguments > 3 then
        send_message(event.player_id, "Usage: /colony status|recruit [role]|role <role> [n]|order <home|hold|follow> [n]|dismiss [n].")
        return
    end
    local action = arguments[1] or "status"
    local argument: string? = arguments[2]
    local index_text = arguments[3]
    local slot: number? = nil
    if index_text ~= nil then
        local parsed = tonumber(index_text)
        if parsed == nil or parsed ~= math.floor(parsed) or parsed < 1 or parsed > config.max_active_members then
            send_message(event.player_id, "Colony command rejected: invalid bounded action.")
            return
        end
        slot = parsed
    end
    if action == "dismiss" and slot == nil and argument ~= nil then
        local parsed = tonumber(argument)
        if parsed ~= nil and parsed == math.floor(parsed) and parsed >= 1 and parsed <= config.max_active_members then
            slot = parsed
            argument = nil
        end
    end
    if (action == "status" and (argument ~= nil or slot ~= nil))
        or (action == "recruit" and ((argument ~= nil and not role_allowed[argument]) or slot ~= nil))
        or (action == "role" and (argument == nil or not role_allowed[argument]))
        or (action == "order" and (argument == nil or not order_allowed[argument]))
        or (action == "dismiss" and argument ~= nil)
        or (action ~= "status" and action ~= "recruit" and action ~= "role" and action ~= "order" and action ~= "dismiss")
    then
        send_message(event.player_id, "Colony command rejected: invalid bounded action.")
        return
    end

    if action == "status" then
        local queued = 0
        for i = 1, config.max_active_members do
            if queue_get(event.player_id, event.uuid, "status_collect", nil, "state-" .. tostring(event.player_id) .. "-" .. tostring(i), member_key(event.uuid, i), i) then
                queued = queued + 1
            end
        end
        if queued == 0 then
            send_message(event.player_id, "Colony status unavailable: request budget exhausted.")
            return
        end
        status_collects[event.player_id] = { remaining = queued, lines = {} }
        return
    end

    local slots: { number } = {}
    if action == "recruit" then
        local free = next_free_slot(event.player_id)
        if free == nil then
            send_message(event.player_id, "Recruitment rejected: member limit reached (" .. tostring(config.max_active_members) .. "). Dismiss a member first.")
            return
        end
        slots = { free }
    elseif slot ~= nil then
        slots = { slot }
    else
        slots = active_slots(event.player_id)
        if #slots == 0 then
            if action == "dismiss" then
                send_message(event.player_id, "Dismiss rejected: no active member.")
            else
                send_message(event.player_id, "Update rejected: recruit an active villager first.")
            end
            return
        end
    end

    for _, member_slot in ipairs(slots) do
        local id = "state-" .. tostring(event.player_id) .. "-" .. tostring(member_slot)
        if queue_get(event.player_id, event.uuid, action, argument, id, member_key(event.uuid, member_slot), member_slot) then
            local pending = assert(pending_gets[id])
            pending.x = event.x
            pending.y = event.y
            pending.z = event.z
        end
    end
end

function on_plugin_storage_get_result(event: any)
    last_batch = nil
    local pending = pending_gets[event.request_id]
    if pending == nil then
        return
    end
    pending_gets[event.request_id] = nil
    if pending.player_id ~= nil then
        local ids = pending_get_by_player[pending.player_id]
        if ids ~= nil then
            ids[event.request_id] = nil
            if next(ids) == nil then
                pending_get_by_player[pending.player_id] = nil
            end
        end
    end
    if event.key ~= pending.key then
        if pending.action == "metadata" then
            startup_state = "metadata_result_mismatch"
        else
            if pending.action == "status_collect" and pending.slot ~= nil then
                collect_status_slot(pending.player_id, nil, pending.slot)
            end
            send_message(pending.player_id, "Colony storage result rejected: correlation mismatch.")
        end
        return
    end
    if event.failure ~= nil then
        if pending.action == "metadata" then
            startup_state = "storage_" .. event.failure
        else
            if pending.action == "status_collect" and pending.slot ~= nil then
                collect_status_slot(pending.player_id, nil, pending.slot)
            end
            send_message(pending.player_id, "Colony storage unavailable: " .. event.failure .. ".")
        end
        return
    end

    if pending.action == "metadata" then
        local expected = metadata_value()
        if event.value == nil then
            queue_cas(
                nil,
                nil,
                pending.key,
                event.version,
                expected,
                nil,
                { kind = "metadata" },
                "persist-colony"
            )
        elseif event.value == expected then
            startup_state = "ready"
            solaris.broadcast(config.colony.name .. " plugin ready.")
        else
            startup_state = "metadata_config_mismatch"
        end
        return
    end

    handle_player_state(pending, event.value, event.version)
end

function on_plugin_storage_cas_result(event: any)
    last_batch = nil
    local pending = pending_cas[event.request_id]
    if pending == nil then
        return
    end
    pending_cas[event.request_id] = nil
    if event.key ~= pending.key then
        if pending.after.kind == "metadata" then
            startup_state = "metadata_result_mismatch"
        else
            local mismatch_player = pending.player_id
            local mismatch_slot = pending.slot
            if mismatch_player ~= nil and mismatch_slot ~= nil then
                clear_cached_slot(mismatch_player, mismatch_slot)
            end
            send_message(pending.player_id, "Colony update rejected: storage result mismatch.")
        end
        return
    end
    if event.failure ~= nil then
        if pending.after.kind == "metadata" then
            startup_state = "storage_" .. event.failure
        else
            send_message(pending.player_id, "Colony update unavailable: " .. event.failure .. ".")
        end
        return
    end
    if not event.applied or event.version == nil then
        if pending.after.kind == "metadata" then
            startup_state = "stale_metadata"
        else
            local stale_player = pending.player_id
            local stale_slot = pending.slot
            if stale_player ~= nil and stale_slot ~= nil then
                clear_cached_slot(stale_player, stale_slot)
            end
            send_message(pending.player_id, "Colony update rejected: stale storage revision.")
        end
        return
    end
    if pending.after.kind == "metadata" then
        startup_state = "ready"
        solaris.broadcast(config.colony.name .. " plugin ready.")
        return
    end

    local cas_player = pending.player_id
    local cas_uuid = pending.uuid
    local cas_slot = pending.slot
    local cas_record = pending.next_record
    if cas_player == nil or cas_uuid == nil or cas_slot == nil or cas_record == nil then
        return
    end
    local members = records[cas_player]
    if members == nil then
        members = {}
        records[cas_player] = members
    end
    members[cas_slot] = {
        uuid = cas_uuid,
        key = pending.key,
        version = event.version,
        value = cas_record,
    }
    if pending.after.kind == "bind" then
        queue_binding({
            player_id = cas_player,
            uuid = cas_uuid,
            key = pending.key,
            slot = cas_slot,
            version = event.version,
            record = cas_record,
            purpose = "recruit",
            retry_binding = false,
            x = pending.after.x,
            y = pending.after.y,
            z = pending.after.z,
        })
    elseif pending.after.kind == "apply_order" then
        local owner_bindings = active_bindings[cas_player]
        local binding = owner_bindings ~= nil and owner_bindings[cas_slot] or nil
        if binding == nil then
            queue_binding({
                player_id = cas_player,
                uuid = cas_uuid,
                key = pending.key,
                slot = cas_slot,
                version = event.version,
                record = cas_record,
                purpose = "apply_order",
                retry_binding = false,
                x = pending.after.x,
                y = pending.after.y,
                z = pending.after.z,
            })
        else
            queue_goal_for_record({
                player_id = cas_player,
                uuid = cas_uuid,
                key = pending.key,
                slot = cas_slot,
                version = event.version,
                record = cas_record,
                purpose = "apply_order",
                binding_expires_at_tick = binding.expires_at_tick,
                retry_binding = true,
                x = pending.after.x,
                y = pending.after.y,
                z = pending.after.z,
            }, binding.lease_id)
        end
    elseif pending.after.kind == "updated" then
        send_message(pending.player_id, "Stored " .. (pending.after.field or "update") .. " intent in Luau storage.")
    elseif pending.after.kind == "binding_complete" then
        send_message(pending.player_id, "Villager recruitment recorded durably by the Luau plugin.")
    elseif pending.after.kind == "binding_rejected" then
        send_message(pending.player_id, "Villager binding failed: " .. (pending.after.failure or "binding_unavailable") .. ".")
    elseif pending.after.kind == "dismissed" then
        local dismissed_player = pending.player_id
        local dismissed_slot = pending.slot
        if dismissed_player ~= nil and dismissed_slot ~= nil then
            clear_slot(dismissed_player, dismissed_slot)
        end
        send_message(pending.player_id, "Member " .. tostring(pending.slot) .. " dismissed: slot released.")
    end
end

function on_villager_binding_result(event: any)
    last_batch = nil
    local pending = pending_bindings[event.request_id]
    if pending == nil then
        return
    end
    pending_bindings[event.request_id] = nil
    if (event.binding_token == nil) ~= (event.binding_expires_at_tick == nil) then
        send_message(pending.player_id, "Binding result rejected: incomplete lease.")
        return
    end
    if event.binding_token == nil then
        local failure = event.failure or "not_found"
        if failure == "busy" and not pending.retry_binding then
            pending.retry_binding = true
            queue_binding(pending)
            return
        end
        if pending.purpose == "refresh_follow" then
            return
        end
        if pending.purpose == "dismiss" then
            send_message(pending.player_id, "Stored dismiss intent, but binding failed: " .. failure .. ".")
            return
        end
        if pending.purpose == "apply_order" then
            send_message(pending.player_id, "Stored order intent, but binding failed: " .. failure .. ".")
            return
        end
        local next_record = copy_record(pending.record)
        next_record.status = "rejected"
        next_record.generation = next_record.generation + 1
        queue_cas(
            pending.player_id,
            pending.uuid,
            pending.key,
            pending.version,
            encode_record(next_record),
            next_record,
            { kind = "binding_rejected", slot = pending.slot, failure = failure },
            "reject"
        )
        return
    end
    pending.binding_expires_at_tick = event.binding_expires_at_tick
    pending.retry_binding = false
    queue_goal_for_record(pending, event.binding_token)
end

function on_villager_goal_result(event: any)
    last_batch = nil
    local pending = pending_goals[event.request_id]
    if pending == nil then
        return
    end
    pending_goals[event.request_id] = nil
    if event.goal ~= expected_goal(pending.record.order) then
        send_message(pending.player_id, "Villager goal result rejected: correlation mismatch.")
        return
    end

    if event.accepted then
        if pending.purpose == "refresh_follow" then
            return
        end
        if pending.purpose == "dismiss" then
            queue_release(pending)
            return
        end
        local bindings = active_bindings[pending.player_id]
        if bindings == nil then
            bindings = {}
            active_bindings[pending.player_id] = bindings
        end
        local stored_lease = pending.lease_id
        local stored_expiry = pending.binding_expires_at_tick
        if stored_lease == nil or stored_expiry == nil then
            return
        end
        bindings[pending.slot] = {
            lease_id = stored_lease,
            expires_at_tick = stored_expiry,
        }
        if pending.purpose == "recruit" then
            local next_record = copy_record(pending.record)
            next_record.status = "active"
            next_record.generation = next_record.generation + 1
            queue_cas(
                pending.player_id,
                pending.uuid,
                pending.key,
                pending.version,
                encode_record(next_record),
                next_record,
                { kind = "binding_complete", slot = pending.slot },
                "activate"
            )
        else
            send_message(pending.player_id, "Applied Luau order " .. pending.record.order .. ".")
        end
        return
    end

    if pending.purpose == "refresh_follow" then
        local refresh_failure = event.failure or "binding_unavailable"
        if refresh_failure == "binding_unavailable" and pending.retry_binding then
            pending.retry_binding = false
            queue_binding(pending)
        end
        return
    end

    if pending.purpose == "dismiss" then
        local dismiss_failure = event.failure or "binding_unavailable"
        if dismiss_failure ~= "binding_unavailable" then
            send_message(pending.player_id, "Dismiss failed: " .. dismiss_failure .. ". Slot retained.")
            return
        end
        local stale_bindings = active_bindings[pending.player_id]
        if stale_bindings ~= nil then
            stale_bindings[pending.slot] = nil
        end
        queue_release(pending)
        return
    end
    local live_bindings = active_bindings[pending.player_id]
    if live_bindings ~= nil then
        live_bindings[pending.slot] = nil
    end
    local failure = event.failure or "binding_unavailable"
    if failure == "binding_unavailable" and pending.retry_binding then
        pending.retry_binding = false
        queue_binding(pending)
        return
    end
    if pending.purpose == "recruit" then
        local next_record = copy_record(pending.record)
        next_record.status = "rejected"
        next_record.generation = next_record.generation + 1
        queue_cas(
            pending.player_id,
            pending.uuid,
            pending.key,
            pending.version,
            encode_record(next_record),
            next_record,
            { kind = "binding_rejected", slot = pending.slot, failure = failure },
            "reject"
        )
    else
        send_message(pending.player_id, "Stored order intent, but goal failed: " .. failure .. ".")
    end
end

function on_villager_release_result(event: any)
    last_batch = nil
    local pending = pending_releases[event.request_id]
    if pending == nil then
        return
    end
    pending_releases[event.request_id] = nil
    if not event.accepted and event.failure ~= "binding_unavailable" then
        send_message(pending.player_id, "Dismiss failed: " .. (event.failure or "binding_unavailable") .. ". Slot retained.")
        return
    end
    local released_record = copy_record(pending.record)
    released_record.status = "released"
    released_record.generation = released_record.generation + 1
    if released_record.generation > config.max_generation then
        send_message(pending.player_id, "Update rejected: generation limit reached.")
        return
    end
    queue_cas(
        pending.player_id,
        pending.uuid,
        pending.key,
        pending.version,
        encode_record(released_record),
        released_record,
        { kind = "dismissed", slot = pending.slot },
        "dismiss"
    )
end

function on_player_entity_interacted(event: any)
    last_batch = nil
    if event.entity_type ~= "minecraft:villager" then
        return
    end
    if player_busy(event.player_id) then
        return
    end
    local members = records[event.player_id]
    if members ~= nil then
        local refreshed = false
        for slot = 1, config.max_active_members do
            local entry = members[slot]
            if entry ~= nil and entry.value ~= nil
                and entry.value.status == "active"
                and entry.value.order == "follow"
            then
                local bindings = active_bindings[event.player_id]
                local binding = bindings ~= nil and bindings[slot] or nil
                if binding ~= nil then
                    refresh_follow_goal(event.player_id, event.uuid, slot, event.x, event.y, event.z)
                    refreshed = true
                end
            end
        end
        if refreshed then
            return
        end
    elseif table_size(records) + table_size(pending_gets) >= config.max_active_players then
        return
    end
    local slot = next_free_slot(event.player_id)
    if slot == nil then
        return
    end
    local id = "state-" .. tostring(event.player_id) .. "-" .. tostring(slot)
    if queue_get(event.player_id, event.uuid, "recruit", nil, id, member_key(event.uuid, slot), slot) then
        local pending = assert(pending_gets[id])
        pending.x = event.x
        pending.y = event.y
        pending.z = event.z
    end
end

function on_player_block_broken(event: any)
    last_batch = nil
    refresh_follow_for_position_event(event.player_id, event.uuid, event.player_x, event.player_y, event.player_z)
end

function on_player_block_placed(event: any)
    last_batch = nil
    refresh_follow_for_position_event(event.player_id, event.uuid, event.player_x, event.player_y, event.player_z)
end

function on_player_left(event: any)
    last_batch = nil
    clear_player(event.player_id)
end

function on_command_batch_rejected(result: any)
    local batch = last_batch
    last_batch = nil
    if batch == nil then
        return
    end
    if batch.kind == "startup" then
        startup_state = "startup_" .. result.reason
        return
    end
    if batch.kind == "message" then
        remember_notice(batch.player_id, "Colony response rejected: " .. result.reason .. ".")
        return
    end
    local batch_request = batch.request_id
    if batch_request == nil then
        return
    end
    if batch.kind == "get" then
        local pending = pending_gets[batch_request]
        pending_gets[batch_request] = nil
        if batch.player_id ~= nil then
            local ids = pending_get_by_player[batch.player_id]
            if ids ~= nil then
                ids[batch_request] = nil
                if next(ids) == nil then
                    pending_get_by_player[batch.player_id] = nil
                end
            end
        elseif pending ~= nil and pending.action == "metadata" then
            startup_state = "storage_" .. result.reason
        end
        if pending ~= nil and pending.action == "status_collect" and pending.slot ~= nil then
            collect_status_slot(pending.player_id, nil, pending.slot)
        end
    elseif batch.kind == "cas" then
        pending_cas[batch_request] = nil
        remember_notice(batch.player_id, "Colony update rejected: " .. result.reason .. ".")
    elseif batch.kind == "binding" then
        pending_bindings[batch_request] = nil
        remember_notice(batch.player_id, "Villager binding request rejected: " .. result.reason .. ".")
    elseif batch.kind == "goal" then
        pending_goals[batch_request] = nil
        remember_notice(batch.player_id, "Villager goal request rejected: " .. result.reason .. ".")
    elseif batch.kind == "release" then
        pending_releases[batch_request] = nil
        remember_notice(batch.player_id, "Villager release request rejected: " .. result.reason .. ".")
    end
end
