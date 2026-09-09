--!strict

-- Shipped item-currency economy for Script API 0.6.
--
-- The catalog mutates currency, purchased items, and its ledger only after the
-- server accepts one inventory/storage transaction.

local config: any = solaris.config()
local shop_id = "economy"
local menu_prefix = "economy"
local max_active_players = 64
local max_purchases_per_item = 999999
local max_ledger_entries = 64

local pending_reads: any = {}
local pending_read_by_player: any = {}
local pending_transactions: any = {}
local ledgers: any = {}
local active_menus: any = {}
local deferred_notices: any = {}
local last_batch: any = nil

local function table_size(values: any): number
    local count = 0
    for _ in pairs(values) do
        count = count + 1
    end
    return count
end

local function validate_config()
    local function bounded_string(value: any, name: string, max_bytes: number)
        assert(type(value) == "string" and #value > 0, name .. " must be a string")
        assert(#value <= max_bytes, name .. " is too long")
    end

    local function resource_id(value: any, name: string)
        bounded_string(value, name, 128)
        assert(string.match(value, "^[a-z0-9_.-]+:[a-z0-9_./-]+$") ~= nil,
            name .. " must be a namespaced resource id")
    end

    local function bounded_integer(value: any, name: string, minimum: number, maximum: number)
        assert(type(value) == "number" and value % 1 == 0, name .. " must be an integer")
        assert(value >= minimum and value <= maximum, name .. " is out of range")
    end

    local function finite_number(value: any, name: string)
        assert(type(value) == "number" and value == value
            and value ~= math.huge and value ~= -math.huge, name .. " must be finite")
    end

    assert(type(config.currency) == "table", "currency must be a table")
    resource_id(config.currency.resource, "currency.resource")
    bounded_string(config.currency.singular, "currency.singular", 128)
    bounded_string(config.currency.plural, "currency.plural", 128)

    assert(type(config.zone) == "table", "zone must be a table")
    bounded_string(config.zone.id, "zone.id", 64)
    assert(string.match(config.zone.id, "^[a-z0-9_-]+$") ~= nil,
        "zone.id contains invalid characters")
    resource_id(config.zone.dimension, "zone.dimension")
    assert(type(config.zone.minimum) == "table", "zone.minimum must be a table")
    assert(type(config.zone.maximum) == "table", "zone.maximum must be a table")
    for _, axis in ipairs({ "x", "y", "z" }) do
        local minimum = config.zone.minimum[axis]
        local maximum = config.zone.maximum[axis]
        finite_number(minimum, "zone.minimum." .. axis)
        finite_number(maximum, "zone.maximum." .. axis)
        local limit = axis == "y" and 20000000 or 30000000
        assert(math.abs(minimum) <= limit and math.abs(maximum) <= limit,
            "zone " .. axis .. " bounds are out of range")
        assert(minimum <= maximum, "zone bounds must be ordered")
    end

    assert(type(config.catalog) == "table", "catalog must be a table")
    assert(#config.catalog > 0 and #config.catalog <= 16, "catalog must contain 1..16 entries")
    local resources = { [config.currency.resource] = true }
    local product_ids = {}
    for _, item in ipairs(config.catalog) do
        assert(type(item) == "table", "catalog item must be a table")
        bounded_string(item.id, "catalog id", 64)
        assert(string.match(item.id, "^[a-z0-9_-]+$") ~= nil,
            "catalog id contains invalid characters")
        resource_id(item.resource, "catalog resource")
        bounded_string(item.label, "catalog label", 48)
        bounded_integer(item.count, "catalog count", 1, 64)
        bounded_integer(item.price, "catalog price", 1, 64)
        assert(item.resource ~= config.currency.resource, "catalog item cannot be the shop currency")
        assert(not product_ids[item.id], "catalog ids must be unique")
        assert(not resources[item.resource], "catalog resources must be unique")
        product_ids[item.id] = true
        resources[item.resource] = true
    end
end

validate_config()

local function ledger_key(uuid: string): string
    return "shop:" .. shop_id .. ":" .. uuid
end

local function copy_records(source: any): any
    local records = {}
    for id, record in pairs(source) do
        records[id] = {
            resource = record.resource,
            item_count = record.item_count,
            price = record.price,
            currency_resource = record.currency_resource,
            purchases = record.purchases,
        }
    end
    return records
end

local function record_matches_item(record: any, item: any): boolean
    return record.resource == item.resource
        and record.item_count == item.count
        and record.price == item.price
        and record.currency_resource == config.currency.resource
end

local function encode_records(records: any): string
    local ids = {}
    for id in pairs(records) do
        ids[#ids + 1] = id
    end
    table.sort(ids)
    local encoded = {}
    for _, id in ipairs(ids) do
        local record = records[id]
        encoded[#encoded + 1] = table.concat({
            id,
            record.resource,
            tostring(record.item_count),
            tostring(record.price),
            record.currency_resource,
            tostring(record.purchases),
        }, ",")
    end
    return "v2|" .. table.concat(encoded, ";")
end

local function decode_records(value: any): any
    if value == nil then
        return {}
    end
    if string.sub(value, 1, 3) ~= "v2|" then
        return nil
    end

    local records = {}
    local payload = string.sub(value, 4)
    if payload == "" then
        return records
    end
    for encoded in string.gmatch(payload, "([^;]+)") do
        local fields = {}
        for field in string.gmatch(encoded, "([^,]+)") do
            fields[#fields + 1] = field
        end
        if #fields ~= 6 then
            return nil
        end
        local id, resource, item_count_text, price_text, currency_resource, purchases_text =
            table.unpack(fields)
        if string.match(id, "^[a-z0-9_-]+$") == nil
            or string.match(resource, "^[a-z0-9_.-]+:[a-z0-9_./-]+$") == nil
            or string.match(currency_resource, "^[a-z0-9_.-]+:[a-z0-9_./-]+$") == nil
            or string.match(item_count_text, "^%d+$") == nil
            or string.match(price_text, "^%d+$") == nil
            or string.match(purchases_text, "^%d+$") == nil
            or records[id] ~= nil
        then
            return nil
        end
        local item_count = tonumber(item_count_text)
        local price = tonumber(price_text)
        local purchases = tonumber(purchases_text)
        if item_count < 1 or item_count > 64
            or price < 1 or price > 64
            or purchases < 1 or purchases > max_purchases_per_item
        then
            return nil
        end
        records[id] = {
            resource = resource,
            item_count = item_count,
            price = price,
            currency_resource = currency_resource,
            purchases = purchases,
        }
        if table_size(records) > max_ledger_entries then
            return nil
        end
    end
    return records
end

local function currency_name(amount: number): string
    if amount == 1 then
        return config.currency.singular
    end
    return config.currency.plural
end

local function menu_id_for(version: any): string
    if version == nil then
        return menu_prefix .. "-new"
    end
    return menu_prefix .. "-v" .. tostring(version)
end

local function remember_notice(player_id: number, message: string)
    deferred_notices[player_id] = message
end

local function send_message(player_id: number, message: string)
    if last_batch == nil then
        last_batch = { kind = "message", player_id = player_id }
    end
    solaris.send_message(player_id, message)
end

local function queue_read(player_id: number, uuid: string, request_id: string, key: string): boolean
    if pending_read_by_player[player_id] ~= nil then
        return false
    end
    local pending = {
        player_id = player_id,
        uuid = uuid,
        key = key,
    }
    pending_reads[request_id] = pending
    pending_read_by_player[player_id] = request_id
    last_batch = { kind = "read", request_id = request_id, player_id = player_id }
    solaris.storage_get(request_id, key)
    return true
end

local function player_has_pending_transaction(player_id: number): boolean
    for _, pending in pairs(pending_transactions) do
        if pending.player_id == player_id then
            return true
        end
    end
    return false
end

local function request_catalog(event: any, request_prefix: string)
    if active_menus[event.player_id] ~= nil
        or pending_read_by_player[event.player_id] ~= nil
        or player_has_pending_transaction(event.player_id)
    then
        return
    end
    if ledgers[event.player_id] == nil
        and table_size(ledgers) + table_size(pending_reads) >= max_active_players
    then
        send_message(event.player_id, "Economy unavailable: active-player limit reached.")
        return
    end

    local notice = deferred_notices[event.player_id]
    if notice ~= nil then
        deferred_notices[event.player_id] = nil
        send_message(event.player_id, notice)
    end
    queue_read(
        event.player_id,
        event.uuid,
        request_prefix .. "-" .. tostring(event.player_id),
        ledger_key(event.uuid)
    )
end

local function open_catalog(player_id: number)
    local ledger = ledgers[player_id]
    if ledger == nil then
        return
    end

    local slots = {}
    for index, item in ipairs(config.catalog) do
        local record = ledger.records[item.id]
        local owned = record == nil and 0 or record.purchases
        local terms = ""
        if record ~= nil and not record_matches_item(record, item) then
            terms = " | changed terms: refund only"
        end
        slots[index] = {
            slot = index - 1,
            resource = item.resource,
            count = item.count,
            label = item.label
                .. " | buy " .. item.price .. " " .. currency_name(item.price)
                .. terms
                .. " | refund | owned " .. owned,
        }
    end

    local menu_id = menu_id_for(ledger.version)
    active_menus[player_id] = menu_id
    last_batch = { kind = "open", player_id = player_id, menu_id = menu_id }
    solaris.open_inventory_menu(
        player_id,
        menu_id,
        "Market - " .. config.currency.plural,
        slots
    )
end

local function clear_player(player_id: number)
    local read_id = pending_read_by_player[player_id]
    if read_id ~= nil then
        pending_reads[read_id] = nil
        pending_read_by_player[player_id] = nil
    end
    for request_id, pending in pairs(pending_transactions) do
        if pending.player_id == player_id then
            pending_transactions[request_id] = nil
        end
    end
    ledgers[player_id] = nil
    active_menus[player_id] = nil
    deferred_notices[player_id] = nil
end

function on_server_started(_event: any)
    last_batch = { kind = "zone_registration" }
    local zone = config.zone
    solaris.upsert_zone(
        zone.id,
        zone.dimension,
        zone.minimum.x,
        zone.minimum.y,
        zone.minimum.z,
        zone.maximum.x,
        zone.maximum.y,
        zone.maximum.z
    )
end

function on_player_command(event: any)
    last_batch = nil
    if event.root ~= "economy" then
        return
    end
    local command = string.match(event.arguments, "^%s*(%S*)%s*$")
    if command ~= "" and command ~= "shop" then
        send_message(event.player_id, "Usage: /economy [shop]")
        return
    end
    request_catalog(event, "command")
end

function on_player_zone_entered(event: any)
    last_batch = nil
    if event.zone_id ~= config.zone.id then
        return
    end
    request_catalog(event, "enter")
end

function on_player_zone_exited(event: any)
    last_batch = nil
    if event.zone_id ~= config.zone.id then
        return
    end
    local read_id = pending_read_by_player[event.player_id]
    if read_id ~= nil then
        pending_reads[read_id] = nil
        pending_read_by_player[event.player_id] = nil
    end
    ledgers[event.player_id] = nil
    local menu_id = active_menus[event.player_id]
    active_menus[event.player_id] = nil
    if menu_id ~= nil then
        last_batch = { kind = "close", player_id = event.player_id }
        solaris.close_inventory_menu(event.player_id, menu_id)
    end
end

function on_plugin_storage_get_result(event: any)
    last_batch = nil
    local pending = pending_reads[event.request_id]
    if pending == nil then
        return
    end
    pending_reads[event.request_id] = nil
    pending_read_by_player[pending.player_id] = nil

    if event.key ~= pending.key then
        send_message(pending.player_id, "Economy unavailable: storage result mismatch.")
        return
    end
    if event.failure ~= nil then
        send_message(
            pending.player_id,
            "Economy unavailable: storage " .. event.failure .. "."
        )
        return
    end

    local records = decode_records(event.value)
    if records == nil then
        send_message(pending.player_id, "Economy unavailable: invalid ledger record.")
        return
    end
    ledgers[pending.player_id] = {
        uuid = pending.uuid,
        key = pending.key,
        version = event.version,
        records = records,
    }
    open_catalog(pending.player_id)
end

function on_inventory_menu_clicked(event: any)
    last_batch = nil
    local ledger = ledgers[event.player_id]
    if ledger == nil or active_menus[event.player_id] ~= event.menu_id then
        return
    end
    local notice = deferred_notices[event.player_id]
    if notice ~= nil then
        deferred_notices[event.player_id] = nil
        send_message(event.player_id, notice)
    end
    if event.click ~= "primary" and event.click ~= "secondary" then
        return
    end

    local item_index = event.slot + 1
    local item = config.catalog[item_index]
    if item == nil then
        return
    end
    for _, pending in pairs(pending_transactions) do
        if pending.player_id == event.player_id then
            return
        end
    end

    local operation = event.click == "primary" and "buy" or "refund"
    local record = ledger.records[item.id]
    if operation == "refund" and record == nil then
        send_message(event.player_id, "Nothing from this shop is eligible for refund.")
        return
    end
    if operation == "buy" and record ~= nil and not record_matches_item(record, item) then
        send_message(event.player_id, "Product terms changed; refund old purchases first.")
        return
    end
    if operation == "buy"
        and record ~= nil
        and record.purchases >= max_purchases_per_item
    then
        send_message(event.player_id, "Purchase limit reached for this product.")
        return
    end

    local next_records = copy_records(ledger.records)
    local inventory
    if operation == "buy" then
        local next_record = next_records[item.id]
        if next_record == nil then
            next_record = {
                resource = item.resource,
                item_count = item.count,
                price = item.price,
                currency_resource = config.currency.resource,
                purchases = 0,
            }
            next_records[item.id] = next_record
        end
        next_record.purchases = next_record.purchases + 1
        inventory = {
            { resource = config.currency.resource, delta = -item.price },
            { resource = item.resource, delta = item.count },
        }
    else
        local next_record = next_records[item.id]
        next_record.purchases = next_record.purchases - 1
        if next_record.purchases == 0 then
            next_records[item.id] = nil
        end
        inventory = {
            { resource = record.currency_resource, delta = record.price },
            { resource = record.resource, delta = -record.item_count },
        }
    end

    local revision = ledger.version == nil and "new" or tostring(ledger.version)
    local request_id = operation
        .. "-" .. tostring(event.player_id)
        .. "-" .. revision
        .. "-" .. tostring(event.slot)
    pending_transactions[request_id] = {
        player_id = event.player_id,
        uuid = event.uuid,
        key = ledger.key,
        operation = operation,
        item_label = item.label,
        old_menu_id = event.menu_id,
    }
    active_menus[event.player_id] = nil
    last_batch = {
        kind = "transaction",
        request_id = request_id,
        player_id = event.player_id,
        old_menu_id = event.menu_id,
    }
    solaris.close_inventory_menu(event.player_id, event.menu_id)
    solaris.inventory_storage_transaction(
        event.player_id,
        request_id,
        inventory,
        {
            {
                operation = "cas",
                key = ledger.key,
                expected_version = ledger.version,
                value = encode_records(next_records),
            },
        }
    )
end

function on_inventory_storage_transaction_result(event: any)
    last_batch = nil
    local pending = pending_transactions[event.request_id]
    if pending == nil then
        return
    end
    pending_transactions[event.request_id] = nil
    ledgers[pending.player_id] = nil

    if event.committed then
        send_message(
            pending.player_id,
            pending.operation == "buy"
                and ("Purchased " .. pending.item_label .. ".")
                or ("Refunded " .. pending.item_label .. ".")
        )
    else
        send_message(
            pending.player_id,
            "Transaction rejected: inventory or storage precondition changed."
        )
    end

    queue_read(
        pending.player_id,
        pending.uuid,
        "refresh-" .. event.request_id,
        pending.key
    )
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

    if batch.kind == "read" then
        pending_reads[batch.request_id] = nil
        pending_read_by_player[batch.player_id] = nil
        remember_notice(batch.player_id, "Economy request rejected: " .. result.reason .. ".")
    elseif batch.kind == "open" then
        active_menus[batch.player_id] = nil
        remember_notice(batch.player_id, "Economy menu rejected: " .. result.reason .. ".")
    elseif batch.kind == "transaction" then
        pending_transactions[batch.request_id] = nil
        active_menus[batch.player_id] = batch.old_menu_id
        remember_notice(batch.player_id, "Economy transaction rejected: " .. result.reason .. ".")
    elseif batch.kind == "message" then
        remember_notice(batch.player_id, "Economy response rejected: " .. result.reason .. ".")
    end
end
